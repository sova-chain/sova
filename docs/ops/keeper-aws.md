# The keeper on AWS

Rob decided on 2026-09-23 that the disclosed keeper miner (infra-2 **D8**,
`docs/ops/keeper-miner.md`) runs on **AWS EC2**. The seed and RPC servers
stay on Hetzner. The launch kit doesn't create anything on AWS and holds
no AWS credentials. Rob creates one instance by hand (steps below) and
hands back its IP address. From then on the kit treats it like any other
host: it logs in over SSH, sets it up, and checks it.

Legend: **[Rob]** = Rob, by hand, in the AWS console. Everything else is
the orchestrator, from `infra/testnet/`.

**What you're making:** one small Ubuntu server named `sova-keeper-1` in
Frankfurt, with a 40 GB disk and a fixed public IP. Nobody on the internet
can connect to it except over SSH from the Mac that runs the kit. About 30
minutes of clicking, and about **$70 a month** [est].

---

## What to create, at a glance

| Setting | Value | Why |
| --- | --- | --- |
| Region | **Europe (Frankfurt) `eu-central-1`** | A few ms from the Hetzner seed (Falkenstein) and RPC (Nuremberg) servers, and in the same (EU) jurisdiction. US East (N. Virginia) works just as well (75 s epochs make 90 ms irrelevant) and is ~$9/month cheaper: your call. |
| Instance type | **`t3a.large`** (2 vCPU, 8 GiB RAM, x86) | See "Why this size" below |
| Image (AMI) | **Ubuntu Server 24.04 LTS (HVM), SSD Volume Type, 64-bit (x86)** | The kit supports only Ubuntu 24.04 on x86_64. **Not Arm**: the release ships no Linux Arm binaries. |
| Disk | **40 GiB gp3**, the root volume (no second volume) | Rob, 2026-09-23. zebrad's testnet state is 12 GB today. |
| Key pair | **`sova-testnet-admin`**, imported from `~/.ssh/sova_testnet_ed25519.pub` | The same key as the Hetzner hosts |
| Security group | **`sova-testnet-keeper`**: inbound SSH from your IP only (and ping) | The same as the Hetzner firewall for private hosts |
| Public IP | An **Elastic IP** | A fixed address. Without it, a stop/start changes the IP, and with it the keeper's P2P identity (enode). |

---

## A. Rob's steps

Do these **on the Mac that runs the kit**, so that "My IP" in step A4 is
the address the kit connects from.

**A1. AWS account** (skip this if Sova already has one).
1. Sign up at aws.amazon.com with the project email. If it asks you to
   choose a **Free** or **Paid** plan, choose **Paid**. A Free-plan account
   stops its resources when its credits or 6 months run out [est: AWS's
   2025 sign-up change], and the keeper has to keep running.
2. **MFA on the root user:** top right, your account name → *Security
   credentials* → *Multi-factor authentication (MFA)* → *Assign MFA
   device*. Use a passkey or hardware key, or an authenticator app.
3. **Budget alarm:** search for *Budgets* → *Create budget* → *Use a
   template* → **Monthly cost budget**. Set the amount to **$100** and add
   your email. The template emails you as the actual or forecast spend
   nears $100.
4. **Don't create access keys.** The kit never calls AWS. Everything
   happens in the console.

*Hand back:* nothing.

**A2. Region.** Top right, next to your account name, switch the region to
**Europe (Frankfurt) eu-central-1**. Key pairs, security groups and IPs
belong to one region, so do everything below in that region.

**A3. Import the SSH key.** Search for **EC2** → left menu *Network &
Security* → **Key Pairs** → *Actions* → **Import key pair**.
- Name: `sova-testnet-admin`
- Key pair file: in Terminal run `pbcopy < ~/.ssh/sova_testnet_ed25519.pub`,
  then paste into the box. It's one line starting with `ssh-ed25519`.
  This is the **public** key (the `.pub` file). Never upload the file
  without `.pub`.
- *Import key pair*.

**A4. Security group (the firewall).** EC2 → *Network & Security* →
**Security Groups** → **Create security group**.
- Name: `sova-testnet-keeper`. Description: `Sova testnet keeper: SSH from admin only`.
  VPC: the default one.
- **Inbound rules**, exactly these two:

  | Type | Protocol | Port range | Source | Description |
  | --- | --- | --- | --- | --- |
  | SSH | TCP | 22 | **My IP** (fills in `x.x.x.x/32`) | `kit admin SSH` |
  | Custom ICMP - IPv4 | Echo Request | — | Anywhere-IPv4 (`0.0.0.0/0`) | `ping` |

- **Outbound rules:** leave the default (*All traffic*, `0.0.0.0/0`). The
  keeper connects out to Zcash peers (tcp/18233) and Sova peers (tcp+udp
  30303), and to GitHub, Docker Hub, Ubuntu's mirrors and Telegram.
- *Create security group*.

Don't add anything else. In particular, never open 30303, 18233, 8545,
8551, 18232 or 18790. The keeper is private: it takes part in both P2P
networks through outbound connections only, just like the Hetzner RPC and
faucet hosts. The ping rule mirrors the Hetzner firewall and is optional.

**A5. Launch the instance.** EC2 → **Instances** → **Launch instances**.
1. *Name and tags*: `sova-keeper-1`.
2. *Application and OS Images*: *Quick Start* → **Ubuntu** → **Ubuntu
   Server 24.04 LTS (HVM), SSD Volume Type**. Architecture: **64-bit
   (x86)**.
3. *Instance type*: **t3a.large**.
4. *Key pair (login)*: **sova-testnet-admin**.
5. *Network settings* → *Edit*: leave the VPC and subnet at their defaults
   and *Auto-assign public IP* at *Enable*. Under *Firewall (security
   groups)* choose **Select existing security group** →
   **sova-testnet-keeper**.
6. *Configure storage*: change `8` to **40** GiB and leave the type at
   **gp3**. Click *Advanced*, then set *Encrypted* to **Encrypted** (the
   default AWS key; it's free).
7. *Advanced details* (expand):
   - *Termination protection*: **Enable**.
   - *Credit specification*: **Unlimited** (usually preset for t3a; see
     "Cost" below).
   - Leave *User data* empty. The kit sets the machine up itself.
8. **Launch instance**.

**A6. Elastic IP.** EC2 → *Network & Security* → **Elastic IPs** →
**Allocate Elastic IP address** → *Allocate*. Select the new address →
*Actions* → **Associate Elastic IP address** → *Resource type*: Instance →
*Instance*: `sova-keeper-1` → **Associate**. (Optional: name it
`sova-keeper-1` in its *Name* column.)

**A7. Check.** EC2 → Instances → `sova-keeper-1`: *Instance state* is
**Running**, *Status check* shows **3/3 checks passed**, and *Public IPv4
address* equals the Elastic IP.

*Hand back:* **the Elastic IP** (for example `3.120.45.67`). It's public,
so chat is fine. Nothing else: no AWS password, no keys.

---

## B. What the orchestrator does with it

1. In `infra/testnet/config.env`, replace the placeholder in the keeper's
   line with the IP:
   ```bash
   "sova-keeper-1:byo:3.120.45.67:40:keeper:ubuntu"
   ```
   The fields are the name, `byo` (bring your own: the kit doesn't create
   it), the address, the disk size (only a check), the role, and the user
   the image lets in first (`ubuntu` on AWS's Ubuntu images).
2. Run `./deploy.sh check`, then `./launch.sh` (or `./provision.sh up
   --my-ip` on its own). Step 2 **adopts** the host:
   - It logs in as `ubuntu` with the kit's key, waits for the image's own
     first boot to finish, then uploads and runs
     `host/byo-bootstrap.py`. That script applies the same
     `host/cloud-init.yaml` a Hetzner server boots with: the `sova-admin`
     user, SSH locked to that user (no passwords, no root, and `ubuntu`
     can't log in any more), ufw default-deny, Docker, unattended upgrades
     and journald caps.
   - It refuses anything but Ubuntu 24.04 on x86_64.
   - It then checks that `sova-admin` logs in, and warns if the disk is
     smaller than the 40 GB in the config.

   From here the keeper is an ordinary kit host. `deploy.sh` sets it up
   (zebrad, `sova` in mine mode signing with the keeper's miner key under
   SIP-6, the `sova-keeper` burner installed but not started, the health
   timer), `launch.sh` waits for its zebrad to
   sync, and `bootnodes.sh --verify` and `smoke.sh` check it.
   `smoke.sh edge` checks from outside that 30303, 18233, 8545, 8551,
   18232 and 18790 are closed on it. That test is what proves the security
   group matches the kit's rules.
3. Its zebrad syncs from zero: about half a day on a fast machine,
   probably 1–2 days on 2 vCPUs [est]. `launch.sh` step 5 waits for it,
   together with the Hetzner hosts.
4. Starting the burner, publishing the disclosure and funding the keeper
   are unchanged: `docs/ops/testnet-launch.md` B5b and
   `docs/ops/keeper-miner.md`.

An offline proof that a byo keeper renders and appears in every launch
stage: `./test/byo-dry-run.sh [--systemd-verify]`.

---

## Why this size

The kit's measured needs (this repo, 2026-09-22/23):

| What | Measured | Source |
| --- | --- | --- |
| zebrad testnet state | **12 GB** | the laptop's synced testnet zebrad (6.3.0) |
| zebrad testnet sync from zero | **~11.7 h** on an 8-core Apple M3 (12:09 → 99.8% at 23:54 UTC) | that node's log |
| zebrad regtest container | 612 MiB | `docker stats`. That's regtest, far below testnet, so it isn't used for sizing |
| `sova` node | ~35 MB RSS | a local sim's node, with a tiny chain. Not used for sizing either |
| The kit's own keeper sizing | Hetzner CX33: 4 vCPU, 8 GB | the previous `config.env.example` |

**Not measured yet:** testnet zebrad's RAM during the sync and at the tip.
Zebra's documentation recommends 16 GB for **mainnet** [ext]. Testnet is
much smaller. So 8 GiB, the same as the CX33 the kit was sized against,
is the smallest size with real headroom.

- **t3a.large (2 vCPU, 8 GiB): recommended.** At the tip the load is
  light: a Zcash block every ~75 s, one Sova epoch per block, and at most
  one small burn per block. That keeps it under the t3a.large baseline
  (30% of each vCPU), so burstable CPU is fine day to day. The one-time
  sync runs above the baseline. In *Unlimited* mode that costs surplus
  credits of about $0.05 per vCPU-hour, so roughly **$2–4, once** [est]. It
  doesn't get throttled.
- **t3a.medium (2 vCPU, 4 GiB): not recommended at launch.** zebrad's
  initial sync, reth's page cache and Docker would share 4 GiB with no
  margin. An out-of-memory kill mid-sync costs more time than the
  ~$31/month it saves. Downsize later if a week of measurements shows the
  box stays under ~2.5 GiB (`free -m`). To resize: stop the instance,
  *Actions → Instance settings → Change instance type*, then start it. The
  Elastic IP, the disk and the keeper's keys all survive.
- **t3.large** (Intel) is the same shape and about 10% more; either is fine.
- **Graviton (t4g, Arm): not possible today.** `box-binaries.yml` builds
  only `darwin-arm64` and `linux-x86_64`. `setup-host.sh` and
  `byo-bootstrap.py` refuse anything but x86_64.

Disk: Ubuntu, Docker and the zebra image take about 6 GB [est]. Add
zebrad's 12 GB (and growing), the small Sova chain, and journald capped at
2 GB, and roughly half of the 40 GB is used at launch. The health timer
alerts at 80%.

## Cost

[est: AWS on-demand list prices for eu-central-1 as of 2026-09. Re-check
on the EC2 pricing page when ordering.]

| Item | Rate | Per month |
| --- | --- | --- |
| t3a.large, 24/7 | ~$0.0864/h × 730 h | ~$63 |
| 40 GB gp3 | ~$0.0952/GB-month | ~$3.80 |
| Public IPv4 (the Elastic IP, attached) | $0.005/h | ~$3.65 |
| Data out | first 100 GB/month free per account, then ~$0.09/GB | ~$0 (a no-inbound node sends perhaps 10–50 GB/month) |
| CPU surplus for the initial sync | ~$0.05/vCPU-hour | ~$2–4, once |
| **Total** | | **≈ $70/month**, plus ~$3 once |

For comparison, the Hetzner CX33 the kit first planned cost about
€12/month. In US East the same box is about $62/month. A 1-year Compute
Savings Plan takes about 30% off, but wait until the keeper has run a
month and the size is settled.

## Operating it

- **Your IP changed** (SSH times out, and `provision.sh up` says so): EC2
  → Security Groups → `sova-testnet-keeper` → *Edit inbound rules* → set
  the SSH rule's source to **My IP** again. The Hetzner side is
  `./provision.sh ssh-allow <cidr>`, which prints this same reminder for
  the keeper.
- **Disk at 80%** (`disk_*` alert): EC2 → *Volumes* → the keeper's volume
  → *Modify* → a new size. Then, on the host:
  `sudo growpart /dev/nvme0n1 1 && sudo resize2fs /dev/nvme0n1p1` (check
  the device names with `lsblk` first).
- **Rebuild:** turn off termination protection, terminate the instance,
  and launch a new one (A5) with the **same Elastic IP** (A6). The
  orchestrator then clears the old host key
  (`ssh-keygen -R <ip> -f infra/testnet/out/known_hosts`) and re-runs
  `./provision.sh up` and `./deploy.sh --only sova-keeper-1`. A new disk
  means a new keeper key (and with it a new sealing key and an empty seal
  journal for it) and a fresh zebrad sync, so it also means a new
  disclosure (`docs/ops/keeper-miner.md`). Don't restore the old
  instance's `/var/lib/sova` onto a new one with the old key while the old
  one might still run: two nodes sealing with one key equivocate.
- **Switch-off drill / teardown:** `./provision.sh teardown` never touches
  AWS. Stop the instance for the drill, or terminate it at the end. Then
  **release the Elastic IP** (Elastic IPs → *Actions* → *Release*),
  because an idle one still costs ~$3.65 a month.
