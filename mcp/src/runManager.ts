// Tracks the (at most one) active `sova-miner mine` child process this
// server has started: pid + log path, kept in-process as the task spec
// calls for. `mine` is long-running (it polls for blocks and blocks on
// confirmation per epoch -- see crates/burn-wallet/miner/src/mine.rs), so
// `sova_mine` starts it detached-from-the-tool-call and returns a run id
// immediately; `sova_status`/`sova_stop` operate on that id afterward.

import { type ChildProcess, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, open, readFile } from "node:fs/promises";
import path from "node:path";
import { MINER_BIN, RUNS_DIR } from "./paths.js";
import { assertMinerBinaryExists } from "./minerCli.js";
import { buildMineArgs, type StartMineArgs } from "./mineArgs.js";

export type { StartMineArgs } from "./mineArgs.js";

export type RunStatus = "running" | "exited" | "stopped" | "killed";

export interface RunRecord {
  runId: string;
  pid: number;
  args: string[];
  command: string;
  logPath: string;
  dataDir: string;
  startedAt: string;
  status: RunStatus;
  exitCode: number | null;
  exitSignal: NodeJS.Signals | null;
  endedAt: string | null;
}

interface InternalRun extends RunRecord {
  child: ChildProcess;
}

// Insertion order == start order; there is at most one entry whose
// `status === "running"` at any time (enforced by startMineRun).
const runs = new Map<string, InternalRun>();

export class RunAlreadyActiveError extends Error {
  constructor(public readonly activeRunId: string) {
    super(
      `a mine run is already active (runId ${activeRunId}). Call sova_stop first, or sova_status to check on it.`,
    );
    this.name = "RunAlreadyActiveError";
  }
}

export class RunNotFoundError extends Error {
  constructor(runId?: string) {
    super(
      runId
        ? `no run found with runId ${runId}`
        : "no mine run has been started yet",
    );
    this.name = "RunNotFoundError";
  }
}

export function findActiveRun(): RunRecord | undefined {
  for (const run of runs.values()) {
    if (run.status === "running") return toPublic(run);
  }
  return undefined;
}

function toPublic(run: InternalRun): RunRecord {
  const { child: _child, ...pub } = run;
  return { ...pub };
}

function mostRecentRun(): InternalRun | undefined {
  let latest: InternalRun | undefined;
  for (const run of runs.values()) latest = run;
  return latest;
}

export async function startMineRun(opts: StartMineArgs): Promise<RunRecord> {
  await assertMinerBinaryExists();

  const active = findActiveRun();
  if (active) throw new RunAlreadyActiveError(active.runId);

  const runId = randomUUID();
  await mkdir(RUNS_DIR, { recursive: true });
  const logPath = path.join(RUNS_DIR, `${runId}.log`);
  const logHandle = await open(logPath, "a");

  const args = buildMineArgs(opts);

  const command = `sova-miner ${args.join(" ")}`;
  const header =
    `[sova-mcp] starting run ${runId} at ${new Date().toISOString()}\n` +
    `[sova-mcp] command: ${command}\n\n`;
  await logHandle.write(header);

  const child = spawn(MINER_BIN, args, {
    stdio: ["ignore", logHandle.fd, logHandle.fd],
    detached: false,
  });

  if (child.pid === undefined) {
    await logHandle.close();
    throw new Error(`failed to spawn ${command}`);
  }

  const record: InternalRun = {
    runId,
    pid: child.pid,
    args,
    command,
    logPath,
    dataDir: opts.dataDir,
    startedAt: new Date().toISOString(),
    status: "running",
    exitCode: null,
    exitSignal: null,
    endedAt: null,
    child,
  };
  runs.set(runId, record);

  child.on("exit", (code, signal) => {
    record.exitCode = code;
    record.exitSignal = signal;
    record.endedAt = new Date().toISOString();
    if (record.status === "running") {
      // Not stopped by us -- the process ended on its own (budget
      // exhausted, --max-epochs reached, or a real error).
      record.status = "exited";
    }
    void logHandle.close();
  });

  return toPublic(record);
}

export function getRun(runId?: string): RunRecord {
  const run = runId ? runs.get(runId) : mostRecentRun();
  if (!run) throw new RunNotFoundError(runId);
  return toPublic(run);
}

function getInternalRun(runId?: string): InternalRun {
  const run = runId ? runs.get(runId) : mostRecentRun();
  if (!run) throw new RunNotFoundError(runId);
  return run;
}

/** Reads a run's full log (for status parsing -- see logParse.ts, which
 * needs the whole thing to get an accurate epoch count, not just the tail
 * a caller asked to see). */
export async function readRunLog(
  runId: string | undefined,
): Promise<{ run: RunRecord; content: string }> {
  const run = getInternalRun(runId);
  let content = "";
  try {
    content = await readFile(run.logPath, "utf8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== "ENOENT") throw err;
  }
  return { run: toPublic(run), content };
}

/**
 * Sends SIGTERM, waits up to `graceMs` for a clean exit, then SIGKILL.
 * A no-op (returns the current record) if the run has already ended.
 */
export async function stopRun(
  runId: string | undefined,
  graceMs = 5_000,
): Promise<RunRecord> {
  const run = getInternalRun(runId);
  if (run.status !== "running") {
    return toPublic(run);
  }

  const exited = new Promise<void>((resolve) => {
    run.child.once("exit", () => resolve());
  });

  run.child.kill("SIGTERM");
  const timedOut = await Promise.race([
    exited.then(() => false),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(true), graceMs)),
  ]);

  if (timedOut && run.status === "running") {
    run.child.kill("SIGKILL");
    await exited;
  }

  run.status = "stopped";
  if (run.endedAt === null) run.endedAt = new Date().toISOString();
  return toPublic(run);
}

export function listRuns(): RunRecord[] {
  return Array.from(runs.values()).map(toPublic);
}
