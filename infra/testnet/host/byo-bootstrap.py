#!/usr/bin/env python3
"""infra/testnet/host/byo-bootstrap.py -- give an optional bring-your-own
host (one provision.sh did not create, e.g. a keeper on AWS or other
hardware; the default is all-Hetzner) the same first-boot base a Hetzner
host gets from host/cloud-init.yaml.

    sudo python3 byo-bootstrap.py <rendered cloud-init.yaml> [--dry-run]

provision.sh copies the rendered cloud-init.yaml (the admin public key
filled in) and this script to the host and runs it as the image's first
login user (e.g. `ubuntu` on AWS). It applies that same file, so there is
one definition of a host's base, not two. Only the cloud-config keys that
file uses are understood; any other key is an error, so the two can't
drift apart silently. Order: users, write_files, packages, runcmd (users
first, so sova-admin can log in before sshd's AllowUsers names it).

Idempotent. After it, SSH accepts only sova-admin (the file's
AllowUsers), and the kit uses sova-admin from then on, as on Hetzner.
Needs PyYAML, which Ubuntu ships with cloud-init (python3-yaml).
"""

import grp
import os
import platform
import pwd
import subprocess
import sys
import tempfile

KNOWN = {"package_update", "package_upgrade", "packages", "users",
         "disable_root", "ssh_pwauth", "write_files", "runcmd"}
DRY = False
APT_ENV = dict(os.environ, DEBIAN_FRONTEND="noninteractive", NEEDRESTART_MODE="a")
APT_OPTS = ["-y", "-q", "-o", "Dpkg::Options::=--force-confdef",
            "-o", "Dpkg::Options::=--force-confold"]


def log(msg):
    print(f"[byo-bootstrap] {msg}", flush=True)


def die(msg):
    print(f"[byo-bootstrap] error: {msg}", file=sys.stderr, flush=True)
    sys.exit(1)


def run(cmd, env=None):
    log("+ " + " ".join(cmd))
    if not DRY:
        subprocess.run(cmd, check=True, env=env)


def write(path, data, mode, uid=0, gid=0):
    log(f"write {path} (mode {mode:o})")
    if DRY:
        return
    d = os.path.dirname(path)
    os.makedirs(d, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=d, prefix=".byo-")
    with os.fdopen(fd, "w") as f:
        f.write(data)
    os.chown(tmp, uid, gid)
    os.chmod(tmp, mode)
    os.replace(tmp, path)


def check_host():
    osr = {}
    with open("/etc/os-release") as f:
        for line in f:
            k, _, v = line.strip().partition("=")
            osr[k] = v.strip('"')
    if osr.get("ID") != "ubuntu" or osr.get("VERSION_ID") != "24.04":
        die(f"Ubuntu 24.04 expected, got {osr.get('ID')} {osr.get('VERSION_ID')}")
    if platform.machine() != "x86_64":
        die(f"x86_64 expected, got {platform.machine()}: the release ships "
            "linux-x86_64 binaries only (pick an x86 instance type and AMI)")


def ensure_user(u, sshd_text):
    name = u["name"]
    groups = u.get("groups") or []
    if isinstance(groups, str):
        groups = [g.strip() for g in groups.split(",")]
    groups = [g for g in groups if _group_exists(g)]
    try:
        pwd.getpwnam(name)
        log(f"user {name} exists")
        if groups:
            run(["usermod", "-a", "-G", ",".join(groups), name])
    except KeyError:
        cmd = ["useradd", "--create-home", "--shell", u.get("shell", "/bin/bash")]
        if u.get("gecos"):
            cmd += ["--comment", u["gecos"]]
        if groups:
            cmd += ["--groups", ",".join(groups)]
        run(cmd + [name])
    if u.get("lock_passwd", True):
        run(["passwd", "--lock", name])
    keys = u.get("ssh_authorized_keys") or []
    if not keys or any("@SSH_PUBLIC_KEY@" in k for k in keys):
        die(f"{name}: no rendered ssh_authorized_keys (pass the rendered cloud-init.yaml)")
    if DRY:
        home, uid, gid = f"/home/{name}", 0, 0
    else:
        pw = pwd.getpwnam(name)
        home, uid, gid = pw.pw_dir, pw.pw_uid, pw.pw_gid
    if not DRY:
        os.makedirs(f"{home}/.ssh", mode=0o700, exist_ok=True)
        os.chown(f"{home}/.ssh", uid, gid)
        os.chmod(f"{home}/.ssh", 0o700)
    write(f"{home}/.ssh/authorized_keys", "".join(k.strip() + "\n" for k in keys), 0o600, uid, gid)
    if u.get("sudo"):
        path = f"/etc/sudoers.d/90-sova-{name}"
        text = f"{name} {u['sudo']}\n"
        if not DRY:
            with tempfile.NamedTemporaryFile("w", delete=False) as t:
                t.write(text)
            subprocess.run(["visudo", "-cq", "-f", t.name], check=True)
            os.unlink(t.name)
        write(path, text, 0o440)
    # AllowUsers must name a user that now exists and has a key.
    if "AllowUsers" in sshd_text and name not in sshd_text:
        die(f"sshd AllowUsers does not include {name}")


def _group_exists(g):
    try:
        grp.getgrnam(g)
        return True
    except KeyError:
        if DRY:
            return True
        log(f"group {g} does not exist; skipped")
        return False


def main():
    global DRY
    args = [a for a in sys.argv[1:] if a != "--dry-run"]
    DRY = len(args) != len(sys.argv[1:])
    if len(args) != 1:
        die("usage: byo-bootstrap.py <rendered cloud-init.yaml> [--dry-run]")
    try:
        import yaml
    except ImportError:
        die("python3-yaml is missing (Ubuntu cloud images ship it with cloud-init): apt-get install python3-yaml")
    with open(args[0]) as f:
        text = f.read()
    if not text.startswith("#cloud-config"):
        die(f"{args[0]} is not a #cloud-config file")
    cfg = yaml.safe_load(text)
    unknown = sorted(set(cfg) - KNOWN)
    if unknown:
        die(f"cloud-init.yaml uses keys this bootstrap does not apply: {', '.join(unknown)} "
            "(teach byo-bootstrap.py, or byo hosts silently differ from Hetzner ones)")
    if os.geteuid() != 0 and not DRY:
        die("run as root (sudo)")
    if not DRY:
        check_host()

    files = cfg.get("write_files") or []
    sshd_text = "".join(f.get("content", "") for f in files if f["path"].startswith("/etc/ssh/"))
    # disable_root / ssh_pwauth are enforced by the sshd drop-in in
    # write_files; refuse a file where that is no longer true.
    if cfg.get("disable_root") and "PermitRootLogin no" not in sshd_text:
        die("disable_root is set but no sshd drop-in says PermitRootLogin no")
    if cfg.get("ssh_pwauth") is False and "PasswordAuthentication no" not in sshd_text:
        die("ssh_pwauth is false but no sshd drop-in says PasswordAuthentication no")

    for u in cfg.get("users") or []:
        if u == "default":
            continue
        ensure_user(u, sshd_text)

    for f in files:
        write(f["path"], f.get("content", ""), int(str(f.get("permissions", "0644")), 8))

    if cfg.get("package_update") or cfg.get("package_upgrade") or cfg.get("packages"):
        run(["apt-get", "update", "-q"], env=APT_ENV)
    if cfg.get("package_upgrade"):
        run(["apt-get", "upgrade"] + APT_OPTS, env=APT_ENV)
    if cfg.get("packages"):
        run(["apt-get", "install"] + APT_OPTS + list(cfg["packages"]), env=APT_ENV)

    for c in cfg.get("runcmd") or []:
        run([str(x) for x in c] if isinstance(c, list) else ["sh", "-c", c])

    log("done: base applied from " + args[0])


if __name__ == "__main__":
    main()
