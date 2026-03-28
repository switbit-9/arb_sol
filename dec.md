# InstructionData Documentation

How the `InstructionData` struct drives the entire arbitrage transaction — from account layout to execution flow.

## Struct Definition

```rust
pub struct InstructionData {
    pub mints: u8,
    pub shared_statics_len: u8,
    pub pool_types: [u8; 8],
    pub type_static_offsets: [u8; 8],
    pub mode: u8,
    pub test: bool,
    pub group_sizes: [u8; 4],
    pub pool_fees: Vec<u32>,
}
```

Deserialized by Anchor in the `initialize` instruction and passed directly to `start_bot()`.

---

## Account Layout

The `remaining_accounts` array is partitioned into four regions using `mints` and `shared_statics_len`:

```
┌──────────────────────────────────────────────────────────────────────────┐
│  FIXED (4)      │  MINTS (mints*2)  │  SHARED STATICS  │  DYNAMIC POOLS │
│  [0..4]         │  [4..4+mints*2]   │  [SS..SS+SSL]    │  [PS..]        │
└──────────────────────────────────────────────────────────────────────────┘
```

### Region 1: Fixed Accounts `[0..4]`

| Index | Account |
|---|---|
| 0 | Payer (signer) |
| 1 | SPL Token program |
| 2 | Token-2022 program |
| 3 | Memo program |

### Region 2: Mint Accounts `[4 .. 4 + mints*2]`

For each mint `i` (0-indexed):
- `accounts[4 + i*2]` = mint account
- `accounts[4 + i*2 + 1]` = user's token account for that mint

### Region 3: Shared Static Accounts

```
shared_statics_start = 4 + mints * 2
```

Contains `shared_statics_len` accounts shared across all pools. Each pool type has its own slice within this block, located via `type_static_offsets[i]`:

```
pool i's static accounts start at: accounts[shared_statics_start + type_static_offsets[i]]
```

Multiple pools of the same DEX type share the same offset (and thus the same static accounts).

### Region 4: Dynamic Pool Accounts

```
pool_start = shared_statics_start + shared_statics_len
```

Each pool's dynamic accounts are laid out contiguously. The span per pool is determined by `dex_type::dynamic_account_count(pool_types[i])`.

```
Pool 0: accounts[pool_start .. pool_start + span_0]
Pool 1: accounts[pool_start + span_0 .. pool_start + span_0 + span_1]
...
```

### Concrete Example

```rust
InstructionData {
    mints: 2,
    shared_statics_len: 13,
    pool_types: [9, 3, 3, 3, 0, 0, 0, 0],
    type_static_offsets: [0, 10, 10, 10, 0, 0, 0, 0],
    ...
}
```

```
[0]       payer
[1-3]     token programs, memo
[4-5]     mint_0, user_ata_0
[6-7]     mint_1, user_ata_1
[8-17]    PumpAmm statics (offset 0, 10 accounts)
[18-20]   Meteora DLMM statics (offset 10, 3 accounts)
[21-28]   Pool 0 dynamic (PumpAmm, 8 accounts)
[29-35]   Pool 1 dynamic (DLMM, 7 accounts)
[36-42]   Pool 2 dynamic (DLMM, 7 accounts)
[43-49]   Pool 3 dynamic (DLMM, 7 accounts)
```

---

## Field Reference

### `mints: u8`

Number of token mints involved. Determines how many mint+ATA pairs appear in Region 2.

- **Single-pair arb (mode 0):** typically `2` (SOL + token)
- **Multi-hop (mode 1):** `3+` (SOL + intermediate tokens)

### `shared_statics_len: u8`

Total number of shared static accounts across all DEX types. Used to calculate `pool_start`.

### `pool_types: [u8; 8]`

DEX type ID for each pool slot. Unused slots are `0`.

| ID | DEX | Dynamic Accounts | Static Accounts | Fee Slots |
|---|---|---|---|---|
| 1 | Meteora DAMM V1 | 13 | 13 | 0 |
| 2 | Meteora DAMM V2 | 3 | 4 | 0 |
| 3 | Meteora DLMM | 7 | 3 | 0 |
| 4 | Orca Whirlpool | 7 | — | 0 |
| 5 | Raydium AMM | 4 | — | 0 |
| 6 | Raydium CLMM | 10 | — | 0 |
| 7 | Raydium CPMM | 5 | — | 0 |
| 8 | Meteora DBC | 13 | 13 | 0 |
| 9 | PumpAmm | 8 | 10 | 1 |

### `type_static_offsets: [u8; 8]`

Per-pool offset into the shared statics block. Pools of the same DEX type share the same offset value, so their static accounts are reused.

```
static_base = shared_statics_start + type_static_offsets[i]
```

### `mode: u8`

Arbitrage execution strategy:

| Value | Constant | Description |
|---|---|---|
| 0 | `SINGLE_PAIR_MULTI_MARKET` | One token pair, multiple DEXes. All pools parsed as one group. |
| 1 | `MULTI_HOP_CHAIN` | Multi-hop chain (e.g. SOL→A→B→SOL). All 8 pool slots parsed. |
| 2 | `MULTIPLE_TRADES` | Multiple independent pair groups. Lazily evaluated — stops at first profitable group. |

**Execution paths:**
- Mode 0 and 2 → `start_bot_grouped()` — iterates over groups defined by `group_sizes`
- Mode 1 → `start_bot_multihop()` — parses all pools, evaluates full chain

### `test: bool`

When `true`:
- Overrides `max_amount_in` to 3 billion lamports
- Overrides execution amount to 1 million lamports (0.001 SOL)
- Skips profit validation checks

### `group_sizes: [u8; 4]`

Partitions pools into up to 4 groups for `MULTIPLE_TRADES` mode. Each value is the pool count for that group.

```
group_sizes = [3, 2, 0, 0]
  → Group 0: pool_types[0..3]  (3 pools)
  → Group 1: pool_types[3..5]  (2 pools)
```

If `group_sizes[0] == 0` or mode is not `MULTIPLE_TRADES`, all active pools are treated as a single group.

Groups are evaluated sequentially — execution stops after the first profitable group.

### `pool_fees: Vec<u32>`

Variable-length fee overrides in millionths (1,000,000 = 100%). Each pool consumes `fee_slot_count(pool_type)` entries from this vector.

- Currently only PumpAmm (type 9) uses 1 fee slot; all others use 0.
- `0` = use on-chain fee from pool state
- `> 0` = override (e.g. `5000` = 0.5%)

Consumed sequentially during `parse_accounts()`:

```
fee_offset starts at 0
for each pool i:
    n_fees = fee_slot_count(pool_types[i])
    pool_fee = pool_fees[fee_offset] if n_fees > 0 else 0
    fee_offset += n_fees
```

---

## Execution Flow

```
initialize()
  └─ start_bot(accounts, data, clock)
       ├─ Compute shared_statics_start, pool_start from mints + shared_statics_len
       ├─ Read payer SOL balance → max_amount_in
       │
       ├─ mode == MULTI_HOP_CHAIN ?
       │    └─ start_bot_multihop()
       │         └─ parse_accounts(0..8) → all pools
       │         └─ evaluate multi-hop chain
       │
       └─ mode == 0 or 2 ?
            └─ start_bot_grouped()
                 └─ for each group in group_sizes:
                      ├─ parse_accounts(group_start..group_end)
                      ├─ find best arb opportunity
                      ├─ if profitable → execute swaps → return
                      └─ else → continue to next group

parse_accounts(pool_idx_start, pool_idx_end)
  └─ for each pool i in range:
       ├─ dex = pool_types[i]
       ├─ span = dynamic_account_count(dex)
       ├─ static_base = shared_statics_start + type_static_offsets[i]
       ├─ pool_fee = pool_fees[fee_offset] (if applicable)
       └─ construct DEX-specific pool instance(accounts, static_base, dyn_start, dyn_end, pool_fee)
```

---

## Client-Side: How to Calculate and Send the Instruction

### Overview

Given a set of pools you want to arbitrage, you derive every `InstructionData` field and the `remaining_accounts` array through the steps below.

### Program & Instruction IDs

- **Program ID:** `BJREZ2NxHAqSf4jeaogmdoyF2nhexVpeewokt5iqqCMt`
- **Instruction:** `initialize`
- **Anchor Discriminator (first 8 bytes):** `[175, 175, 109, 31, 13, 152, 155, 237]`
  - Computed as `sha256("global:initialize")[0..8]`

---

### Step 1: Choose `mode`

| Scenario | `mode` | Description |
|---|---|---|
| One token pair, multiple DEX pools | `0` | e.g. SOL/TOKEN across PumpAmm + DLMM + Orca |
| Chain through different tokens | `1` | e.g. SOL → A → B → SOL |
| Multiple independent pairs, lazy eval | `2` | Evaluates groups sequentially, stops at first profitable |

### Step 2: Collect unique mints → `mints`

Gather all unique token mints across your selected pools.

- Mode 0: typically `2` (WSOL + token)
- Mode 1: `3+` (WSOL + each intermediate token)
- Mode 2: `2` per group (but deduplicated across all groups)

**Critical:** The first mint+ATA pair at `accounts[4..5]` **must be WSOL**. The program reads `accounts[5]` (the WSOL token account) to determine `max_amount_in` and to verify profit after execution.

```
mints = count of unique mints (WSOL always included)
```

### Step 3: Assign `pool_types[0..N]`

For each pool (up to 8), set the DEX type ID. Unused slots stay `0`.

| ID | DEX | Static Accounts | Dynamic Accounts | Fee Slots |
|---|---|---|---|---|
| 1 | Meteora DAMM V1 | 1 | 13 | 0 |
| 2 | Meteora DAMM V2 | 4 | 3 | 0 |
| 3 | Meteora DLMM | 3 | 7 | 0 |
| 4 | Orca Whirlpool | 1 | 7 | 0 |
| 5 | Raydium AMM | 2 | 4 | 0 |
| 6 | Raydium CLMM | 1 | 10 | 0 |
| 7 | Raydium CPMM | 2 | 5 | 0 |
| 8 | Meteora DBC | 1 | 13 | 0 |
| 9 | PumpAmm | 10 | 8 | 1 |

**For mode 2:** pools must be ordered by group (all pools in group 0 first, then group 1, etc.).

### Step 4: Build shared statics → `shared_statics_len` + `type_static_offsets`

Pools of the same DEX type share the same static accounts. The client deduplicates by DEX type:

```
offset = 0
seen_types = {}
statics_accounts = []

for each pool i (0..N):
    dex = pool_types[i]
    if dex in seen_types:
        type_static_offsets[i] = seen_types[dex]
    else:
        type_static_offsets[i] = offset
        seen_types[dex] = offset
        statics_accounts.append(...get_static_accounts(dex))
        offset += static_account_count(dex)

shared_statics_len = offset
```

**Example** — 1 PumpAmm pool + 3 DLMM pools:

```
pool_types = [9, 3, 3, 3, 0, 0, 0, 0]

Pool 0 (PumpAmm, type 9):  first time → offset=0,  advance by 10 → offset=10
Pool 1 (DLMM, type 3):     first time → offset=10, advance by 3  → offset=13
Pool 2 (DLMM, type 3):     already seen → reuse offset 10
Pool 3 (DLMM, type 3):     already seen → reuse offset 10

Result:
  type_static_offsets = [0, 10, 10, 10, 0, 0, 0, 0]
  shared_statics_len  = 13
```

#### Static Accounts Per DEX Type

Each DEX type's static accounts must appear in the shared statics region at the assigned offset, in this exact order:

**PumpAmm (type 9) — 10 accounts:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn3gaP2rYc`) |
| S+1 | Protocol fee recipient |
| S+2 | Protocol fee recipient token account |
| S+3 | Event authority |
| S+4 | Fee config |
| S+5 | Fee program |
| S+6 | Pump AMM global |
| S+7 | System program |
| S+8 | Associated token program |
| S+9 | Global volume accumulator |

**Meteora DAMM V2 (type 2) — 4 accounts:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`) |
| S+1 | Pool authority |
| S+2 | Event authority |
| S+3 | Referral token account |

**Meteora DLMM (type 3) — 3 accounts:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`) |
| S+1 | Host fee in (same as program ID when no host fee) |
| S+2 | Event authority |

**Raydium AMM (type 5) — 2 accounts:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`) |
| S+1 | AMM authority |

**Raydium CPMM (type 7) — 2 accounts:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`) |
| S+1 | Vault authority |

**Meteora DAMM V1 (type 1) / Meteora DBC (type 8) — 1 account:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB`) |

**Orca Whirlpool (type 4) — 1 account:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc`) |

**Raydium CLMM (type 6) — 1 account:**

| Offset | Account |
|---|---|
| S+0 | Program ID (`CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`) |

### Step 5: Build dynamic pool accounts (Region 4)

Each pool's unique accounts are laid out contiguously after the shared statics. The program walks them in order using `dynamic_account_count(pool_type)`.

#### Dynamic Accounts Per DEX Type

**PumpAmm (type 9) — 8 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool |
| D+1 | Base vault |
| D+2 | Quote vault |
| D+3 | User volume accumulator |
| D+4 | User volume WSOL ATA |
| D+5 | Vault ATA |
| D+6 | Vault authority |
| D+7 | Cashback pool ID |

**Meteora DAMM V1 / DBC (types 1, 8) — 13 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool |
| D+1 | A vault (vault state) |
| D+2 | B vault (vault state) |
| D+3 | A token vault |
| D+4 | B token vault |
| D+5 | A vault LP mint |
| D+6 | B vault LP mint |
| D+7 | A vault LP (pool's LP in vault A) |
| D+8 | B vault LP (pool's LP in vault B) |
| D+9 | Protocol fee A |
| D+10 | Protocol fee B |
| D+11 | Vault program |
| D+12 | Token program |

**Meteora DAMM V2 (type 2) — 3 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool |
| D+1 | Base vault |
| D+2 | Quote vault |

**Meteora DLMM (type 3) — 7 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool (lb_pair) |
| D+1 | Base vault |
| D+2 | Quote vault |
| D+3 | Oracle |
| D+4 | Bitmap extension |
| D+5 | Bin array (buy direction) |
| D+6 | Bin array (sell direction) |

**Orca Whirlpool (type 4) — 7 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool (whirlpool) |
| D+1 | Vault A |
| D+2 | Vault B |
| D+3 | Oracle |
| D+4 | Tick array 0 |
| D+5 | Tick array 1 |
| D+6 | Tick array 2 |

**Raydium AMM (type 5) — 4 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool (AMM state) |
| D+1 | Coin vault (base token) |
| D+2 | PC vault (quote token) |
| D+3 | Open orders (Serum/OpenBook) |

**Raydium CLMM (type 6) — 10 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool (pool state) |
| D+1 | Vault 0 |
| D+2 | Vault 1 |
| D+3 | AMM config |
| D+4 | Observation (oracle) |
| D+5 | Bitmap extension |
| D+6 | Tick array (buy, primary) |
| D+7 | Tick array (buy, secondary) |
| D+8 | Tick array (sell, primary) |
| D+9 | Tick array (sell, secondary) |

**Raydium CPMM (type 7) — 5 accounts:**

| Offset | Account |
|---|---|
| D+0 | Pool (pool state) |
| D+1 | Base vault |
| D+2 | Quote vault |
| D+3 | AMM config |
| D+4 | Observation (oracle) |

### Step 6: Calculate `group_sizes` (mode 2 only)

For mode `0`: set `[0, 0, 0, 0]` — the program auto-counts active pools into one group.

For mode `1`: ignored — all 8 pool slots are parsed.

For mode `2` (MULTIPLE_TRADES): partition your pools into up to 4 groups. Each value is the pool count for that group. Pools in `pool_types` must be ordered by group.

```
group_sizes = [3, 2, 0, 0]
  → Group 0: pool_types[0..3]  (3 pools for pair A)
  → Group 1: pool_types[3..5]  (2 pools for pair B)
```

Groups are evaluated sequentially — execution stops after the first profitable group.

### Step 7: Build `pool_fees`

Iterate pools in order. Each pool consumes `fee_slot_count(pool_type)` entries from the vector:

- PumpAmm (type 9): **1 slot** — `0` = use on-chain fee, `> 0` = override in millionths (e.g. `5000` = 0.5%)
- All other types: **0 slots** — skip

```
pool_types = [9, 3, 3, 3, ...]
pool_fees  = [5000]   // only PumpAmm consumes a slot
                       // DLMM pools consume 0 slots each

pool_types = [3, 3, 0, 0, ...]
pool_fees  = []        // no pools use fee slots
```

### Step 8: Set `test`

- `true`: forces swap amount to 0.001 SOL, skips profit validation. For development/testing.
- `false`: production mode. Uses real balance, enforces profitability.

---

### Complete Client Pseudocode

```
function buildInstruction(pools, mode):
    // Step 2: Unique mints (WSOL must be first)
    mints = dedup([WSOL, ...all base/quote mints from pools])

    // Step 3: Pool types
    pool_types = [0; 8]
    for i, pool in enumerate(pools):
        pool_types[i] = pool.dex_type_id

    // Step 4: Shared statics with deduplication
    offset = 0
    seen = {}
    type_static_offsets = [0; 8]
    statics_accounts = []
    for i, pool in enumerate(pools):
        dex = pool_types[i]
        if dex not in seen:
            seen[dex] = offset
            statics_accounts.extend(get_static_accounts_for_dex(dex))
            offset += static_account_count(dex)
        type_static_offsets[i] = seen[dex]
    shared_statics_len = offset

    // Step 5: Dynamic accounts (contiguous, in pool order)
    dynamic_accounts = []
    for pool in pools:
        dynamic_accounts.extend(get_dynamic_accounts_for_pool(pool))

    // Step 6: Group sizes
    group_sizes = [0, 0, 0, 0]
    if mode == 2:
        for g, group in enumerate(pool_groups):
            group_sizes[g] = len(group)

    // Step 7: Fees
    pool_fees = []
    for pool in pools:
        n = fee_slot_count(pool.dex_type_id)
        if n > 0:
            pool_fees.push(pool.fee_override or 0)

    // Build remaining_accounts array
    remaining_accounts = [
        payer,                                                // [0]
        SPL_TOKEN_PROGRAM, TOKEN_2022_PROGRAM, MEMO_PROGRAM,  // [1-3]
        ...flatten(mints.map(m => [m.mint, m.ata])),           // [4 .. 4+mints*2)
        ...statics_accounts,                                   // shared statics region
        ...dynamic_accounts,                                   // per-pool dynamic region
    ]

    instruction_data = InstructionData {
        mints: len(mints),
        shared_statics_len,
        pool_types,
        type_static_offsets,
        mode,
        test: false,
        group_sizes,
        pool_fees,
    }

    return { instruction_data, remaining_accounts }
```

---

### Serialization Format (Borsh)

Anchor uses Borsh serialization. The instruction data buffer sent on-chain is:

```
┌────────────────────────────────────────────────────────────────────────┐
│ Discriminator (8 bytes) │ Borsh-serialized InstructionData            │
└────────────────────────────────────────────────────────────────────────┘
```

**Binary layout of `InstructionData` (all little-endian):**

| Offset | Size | Field | Encoding |
|---|---|---|---|
| 0 | 1 | `mints` | u8 |
| 1 | 1 | `shared_statics_len` | u8 |
| 2 | 8 | `pool_types` | [u8; 8] raw bytes |
| 10 | 8 | `type_static_offsets` | [u8; 8] raw bytes |
| 18 | 1 | `mode` | u8 |
| 19 | 1 | `test` | u8 (0 = false, 1 = true) |
| 20 | 4 | `group_sizes` | [u8; 4] raw bytes |
| 24 | 4 | `pool_fees.len()` | u32 LE (number of elements) |
| 28 | 4×N | `pool_fees` data | N × u32 LE |

**Total size:** `28 + 4 * pool_fees.len()` bytes (plus 8-byte discriminator prefix).

### Accounts (Anchor Context)

The `Initialize` context struct has **no named accounts** — everything is passed via `remaining_accounts`. The payer must be a signer.

### Account Writability / Signer Rules

| Region | Signer | Writable |
|---|---|---|
| `[0]` Payer | Yes | Yes |
| `[1-3]` Programs | No | No |
| Mint accounts | No | No |
| User ATAs | No | Yes |
| Shared statics | No | Depends on DEX |
| Dynamic pool accounts | No | Depends on DEX |

---

### Concrete Walkthrough

Suppose you want to arbitrage SOL/TOKEN across 1 PumpAmm pool and 3 Meteora DLMM pools (mode 0):

**Computed fields:**

```
mints                = 2          (WSOL + TOKEN)
mode                 = 0          (single pair, multi market)
test                 = false
pool_types           = [9, 3, 3, 3, 0, 0, 0, 0]
type_static_offsets  = [0, 10, 10, 10, 0, 0, 0, 0]
shared_statics_len   = 13         (10 PumpAmm + 3 DLMM)
group_sizes          = [0, 0, 0, 0]  (mode 0, auto)
pool_fees            = [5000]     (PumpAmm fee override 0.5%)
```

**`remaining_accounts` layout (50 accounts total):**

```
Index    Region              Account
─────    ──────              ───────
[0]      Fixed               Payer (signer)
[1]      Fixed               SPL Token program
[2]      Fixed               Token-2022 program
[3]      Fixed               Memo program
[4]      Mint 0              WSOL mint
[5]      Mint 0              User's WSOL ATA
[6]      Mint 1              TOKEN mint
[7]      Mint 1              User's TOKEN ATA
[8]      PumpAmm statics     Program ID
[9]      PumpAmm statics     Protocol fee recipient
[10]     PumpAmm statics     Protocol fee recipient token acc
[11]     PumpAmm statics     Event authority
[12]     PumpAmm statics     Fee config
[13]     PumpAmm statics     Fee program
[14]     PumpAmm statics     Pump AMM global
[15]     PumpAmm statics     System program
[16]     PumpAmm statics     Associated token program
[17]     PumpAmm statics     Global volume accumulator
[18]     DLMM statics        Program ID
[19]     DLMM statics        Host fee in
[20]     DLMM statics        Event authority
[21]     Pool 0 (PumpAmm)    Pool
[22]     Pool 0 (PumpAmm)    Base vault
[23]     Pool 0 (PumpAmm)    Quote vault
[24]     Pool 0 (PumpAmm)    User volume accumulator
[25]     Pool 0 (PumpAmm)    User volume WSOL ATA
[26]     Pool 0 (PumpAmm)    Vault ATA
[27]     Pool 0 (PumpAmm)    Vault authority
[28]     Pool 0 (PumpAmm)    Cashback pool ID
[29]     Pool 1 (DLMM)       Pool (lb_pair)
[30]     Pool 1 (DLMM)       Base vault
[31]     Pool 1 (DLMM)       Quote vault
[32]     Pool 1 (DLMM)       Oracle
[33]     Pool 1 (DLMM)       Bitmap extension
[34]     Pool 1 (DLMM)       Bin array (buy)
[35]     Pool 1 (DLMM)       Bin array (sell)
[36]     Pool 2 (DLMM)       Pool (lb_pair)
[37]     Pool 2 (DLMM)       Base vault
[38]     Pool 2 (DLMM)       Quote vault
[39]     Pool 2 (DLMM)       Oracle
[40]     Pool 2 (DLMM)       Bitmap extension
[41]     Pool 2 (DLMM)       Bin array (buy)
[42]     Pool 2 (DLMM)       Bin array (sell)
[43]     Pool 3 (DLMM)       Pool (lb_pair)
[44]     Pool 3 (DLMM)       Base vault
[45]     Pool 3 (DLMM)       Quote vault
[46]     Pool 3 (DLMM)       Oracle
[47]     Pool 3 (DLMM)       Bitmap extension
[48]     Pool 3 (DLMM)       Bin array (buy)
[49]     Pool 3 (DLMM)       Bin array (sell)
```
