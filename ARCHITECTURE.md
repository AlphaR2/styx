# Styx Architecture

## System Overview

Styx is organized as a Rust workspace of five crates with clear separation between network ingestion, core transaction logic, AI decision-making, and the user-facing API. Data flows in one direction: the network feeds the ingest layer, the ingest layer feeds the core layer via an event bus, and the agent layer reads from both to make decisions and drive execution.

## Crate Dependency Graph

```mermaid
graph TD
    ingest["ingest<br/>Yellowstone subscriber<br/>network event bus"]
    core["core<br/>auction window, bid types<br/>compose, retry, lifecycle"]
    agent["agent<br/>LLM classifier, baseline<br/>execute engine"]
    styx["styx<br/>public SDK re-exports"]
    demo["demo<br/>Axum API, WebSocket<br/>UI backend"]

    ingest --> core
    ingest --> agent
    core --> agent
    agent --> styx
    core --> styx
    ingest --> styx
    styx --> demo
    core --> demo
    ingest --> demo
```

## Data Flow

```mermaid
flowchart LR
    YS["Yellowstone<br/>gRPC Stream"]
    subgraph ingest ["ingest crate"]
        SUB["subscriber.rs<br/>slot updates<br/>payer tx filter<br/>Jito tip accounts"]
        BUS["broadcast bus<br/>NetworkEvent"]
    end
    subgraph core ["core crate"]
        AW["AuctionWindow<br/>20-slot ring buffer<br/>clearing price stats<br/>regime detection"]
        LT["LifecycleTracker<br/>per-bundle stage map<br/>slot and timestamp records"]
        RL["RetryLoop<br/>failure classification<br/>blockhash refresh<br/>bundle resubmission"]
    end
    subgraph agent ["agent crate"]
        CL["LlmClassifier<br/>forward_multiplier decision<br/>retry advice"]
        BL["OverpayerBaseline<br/>deterministic 2x bid<br/>savings benchmark"]
        EX["execute.rs<br/>prepare and submit<br/>outcome recording"]
    end
    subgraph external ["external services"]
        JITO["Jito Block Engine<br/>4 regions concurrent"]
        RPC["Solana RPC<br/>blockhash, confirmation<br/>fallback polling"]
    end

    YS --> SUB
    SUB --> BUS
    BUS --> AW
    BUS --> LT
    BUS --> EX
    AW --> EX
    EX --> CL
    EX --> BL
    CL --> EX
    BL --> EX
    EX --> JITO
    EX --> RPC
    JITO --> RL
    RPC --> RL
    RL --> CL
    CL --> RL
    RL --> JITO
```

## Bundle Lifecycle State Machine

```mermaid
stateDiagram-v2
    [*] --> Preparing : POST /execute

    Preparing --> Submitted : bundle signed and sent to Jito

    Submitted --> Processed : Yellowstone payer tx event<br/>OR RPC status check

    Processed --> Confirmed : supermajority vote observed<br/>via Yellowstone slot update

    Confirmed --> Finalized : slot rooted<br/>cannot be rolled back

    Submitted --> RetryEval : 60s timeout with no confirmation

    RetryEval --> FailureClassify : detect failure kind

    FailureClassify --> AgentAdvise : pass RetrySignal to LLM

    AgentAdvise --> BlockhashRefresh : refresh_blockhash = true

    BlockhashRefresh --> Resubmitted : fresh blockhash<br/>recalculated tip<br/>new bundle ID

    Resubmitted --> Confirmed : confirmed after retry

    Resubmitted --> RetryEval : still no confirmation<br/>up to MAX_RETRIES = 3

    AgentAdvise --> Terminal : agent returns Abort<br/>OR unrecoverable failure kind

    RetryEval --> Exhausted : retries >= MAX_RETRIES

    Finalized --> [*]
    Terminal --> [*]
    Exhausted --> [*]
```

## Tip Pricing Pipeline

```mermaid
flowchart TD
    TA["Jito Tip Accounts<br/>8 addresses monitored<br/>via Yellowstone"]
    DELTA["Balance delta observed<br/>tip_lamports = new - old<br/>only increases recorded"]
    INGEST["AuctionWindow.ingest<br/>slot, tip_lamports"]
    RING["20-slot ring buffer<br/>per-slot max = clearing price"]
    STATS["Statistics recomputed<br/>min, median, max<br/>bundles_per_slot<br/>trend, regime"]
    BASELINE["compute_baseline<br/>= clearing_price_median<br/>clamped to MIN_TIP_LAMPORTS"]
    SAFETY["safety_margin<br/>Cold 1.05<br/>Warm 1.10<br/>Hot 1.20<br/>Manic 1.50"]
    LLM["LLM Agent<br/>forward_multiplier<br/>0.1 to 10.0"]
    FORMULA["tip = baseline x safety x forward_multiplier"]
    VCAP["value_cap by TxType<br/>Snipe 80%<br/>Swap 5%<br/>Arb 60%<br/>Memo unlimited"]
    CEIL["ceiling = min(config_ceiling, value_cap)<br/>floored at MIN_TIP_LAMPORTS"]
    FINAL["final tip lamports"]

    TA --> DELTA --> INGEST --> RING --> STATS
    STATS --> BASELINE --> FORMULA
    STATS --> SAFETY --> FORMULA
    LLM --> FORMULA
    FORMULA --> VCAP --> CEIL --> FINAL
```

## Retry Decision Flow

```mermaid
flowchart TD
    TIMEOUT["60s confirmation timeout"]
    CLASSIFY["failure::detect<br/>check blockhash age<br/>check on-chain status<br/>inspect error message"]

    CLASSIFY --> EXPIRED["ExpiredBlockhash"]
    CLASSIFY --> FEETOOLOW["FeeTooLow"]
    CLASSIFY --> DROPPED["Dropped"]
    CLASSIFY --> COMPUTE["ComputeExceeded"]
    CLASSIFY --> BUNDLEFAIL["BundleFailure"]

    EXPIRED --> SIGNAL["RetrySignal to LLM<br/>failure_kind<br/>attempt number<br/>previous tip and multiplier<br/>current AuctionWindow<br/>seconds elapsed"]
    FEETOOLOW --> SIGNAL
    DROPPED --> SIGNAL

    COMPUTE --> TERMINAL["Terminal: agent aborts<br/>no retry possible"]
    BUNDLEFAIL --> TERMINAL

    SIGNAL --> ADVICE["RetryAdvice from LLM<br/>action: Retry or Abort<br/>forward_multiplier<br/>refresh_blockhash<br/>reasoning"]

    ADVICE --> ABORT_CHECK{action == Abort?}
    ABORT_CHECK --> |yes| TERMINAL
    ABORT_CHECK --> |no| FRESH["fetch fresh blockhash<br/>confirmed commitment"]

    FRESH --> REPRICE["compute_tip with new multiplier<br/>enforce prev_tip + 1 if FeeTooLow or Dropped"]
    REPRICE --> RESIGN["rebuild and re-sign bundle"]
    RESIGN --> SUBMIT["send to all Jito regions"]
    SUBMIT --> WAIT["wait_confirmed 30s"]

    WAIT --> |confirmed| SUCCESS["RetryOutcome::Confirmed"]
    WAIT --> |timeout| RETRY_CHECK{retries < MAX_RETRIES?}
    RETRY_CHECK --> |yes| TIMEOUT
    RETRY_CHECK --> |no| EXHAUSTED["RetryOutcome::Exhausted"]
```

## Network Event Bus

```mermaid
flowchart LR
    PUB["subscriber.rs<br/>publishes"]

    PUB --> E1["SlotUpdate<br/>slot, parent, commitment"]
    PUB --> E2["TxSeen<br/>sig, slot"]
    PUB --> E3["JitoTip<br/>slot, tip_lamports, ts_ms"]
    PUB --> E5["Execution<br/>bundle_id, stage, tip, regime"]
    PUB --> E6["ExecLog<br/>bundle_id, level, message"]

    E1 --> C1["LeaderClock<br/>slot tracking"]
    E1 --> C2["LifecycleTracker<br/>commitment progression"]
    E3 --> C3["AuctionWindow<br/>tip ingestion"]
    E5 --> C5["WebSocket clients"]
    E5 --> C6["bundle replay buffer"]
    E6 --> C5
    E6 --> C6
```

## Commitment Level Usage

```mermaid
flowchart TD
    A["blockhash fetch for new bundle"] --> CONF["confirmed commitment<br/>1-2 slots lag<br/>maximizes validity window"]
    B["blockhash fetch for retry"] --> CONF
    C["balance check on startup"] --> CONF
    D["transaction status polling fallback"] --> PROC["processed commitment<br/>fastest signal<br/>not final"]
    E["transaction simulation"] --> PROC
    F["finalization wait"] --> FINAL["finalized commitment<br/>31-32 slots lag<br/>used only to record<br/>finalized_at_ms in log"]
```

## Infrastructure Connections

```mermaid
graph LR
    STYX["Styx demo process"]

    STYX <-->|"gRPC TLS stream<br/>slots + txs + accounts"| YS["Yellowstone<br/>SolInfra"]
    STYX <-->|"JSON-RPC HTTPS<br/>blockhash, balance, status"| RPC["Solana RPC<br/>SolInfra"]
    STYX -->|"HTTP POST /bundles<br/>4 regions concurrent"| J1["Jito Frankfurt"]
    STYX -->|"HTTP POST /bundles"| J2["Jito Amsterdam"]
    STYX -->|"HTTP POST /bundles"| J3["Jito New York"]
    STYX -->|"HTTP POST /bundles"| J4["Jito Tokyo"]
    STYX <-->|"HTTP POST /chat/completions<br/>OpenAI-compatible"| LLM["LLM Provider<br/>Together / OpenAI / Groq<br/>Anthropic / Ollama"]
    STYX <-->|"HTTP GET /quote /swap"| JUP["Jupiter Aggregator"]
```
