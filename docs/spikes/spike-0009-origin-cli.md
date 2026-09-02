# Origin CLI

> **Sources:** [getorigin.io](https://getorigin.io/) · [Documentation](https://getorigin.io/docs) ·
> [CLI command reference](https://getorigin.io/docs/cli/commands) ·
> [GitHub: opsworks-co/origin-cli](https://github.com/opsworks-co/origin-cli)

## Overview

[Origin](https://getorigin.io/) is an AI coding activity and provenance tool that makes work
performed by AI coding agents visible, traceable, and attributable to a Git repository.

Its core idea is **AI coding provenance**: connecting the development chain:

```mermaid
flowchart LR
    A[Developer] --> B[AI coding agent]
    B --> C[Prompt / session]
    C --> D[Tools and files]
    D --> E[Generated changes]
    E --> F[Git commit]
    F --> G[Source-code lines]
```

Origin is designed to work with AI coding environments such as Claude Code, Codex CLI, Cursor,
Gemini CLI, GitHub Copilot, Aider, Windsurf, Antigravity, and other supported agents.

## Main Features

### AI Session Tracking

Origin can capture:

- AI agent and model
- sessions and prompts
- prompt sequence
- tools used
- files read and modified
- token usage
- estimated cost
- session state
- generated diffs

### Prompt Tracking

Origin captures prompts submitted to supported agents and associates prompts with activity
occurring during the corresponding AI session.

This makes it possible to investigate both:

> What did the developer ask the AI to do?

and:

> What code resulted from that request?

### Agent Hooks and Transcript Parsing

Origin uses a combination of **native agent hooks and transcript parsing**.

Different AI agents expose different mechanisms, so Origin has agent-specific integrations that
normalize activity into a common representation.

Conceptually:

```mermaid
flowchart LR
    A[Claude transcript] --> N[Normalized Origin session]
    B[Codex transcript] --> N
    C[Cursor events] --> N
    D[Gemini events] --> N
    E[Copilot events] --> N
```

Supported lifecycle events can include:

- session start
- prompt submission
- tool execution
- file edits
- agent completion
- session end

Origin also watches agent transcript files so that session information can be recovered when
lifecycle hooks are insufficient.

## Git Integration

Git is fundamental to Origin.

Origin combines information reported by the AI agent with the actual state of the Git working
tree and commit history.

```mermaid
flowchart LR
    subgraph Agent["AI agent"]
        A[What happened during the AI session?]
    end
    subgraph Git["Git"]
        B[What actually changed?]
    end
    A --> C[Connect AI activity to real source-code changes]
    B --> C
```

This makes it possible to connect AI activity to real source-code changes.

## Git Notes and Origin Metadata

Origin uses Git metadata facilities to associate AI provenance with repository history.

The primary ref is:

```text
refs/notes/origin
```

which stores per-commit model / session / cost / token metadata. Origin also maintains
session-related data on the `origin-sessions` **branch**, which carries transcripts, prompts,
and file changes.

```mermaid
flowchart LR
    subgraph Notes["Git notes"]
        N1[refs/notes/origin<br/>per-commit metadata]
    end
    subgraph Branch["Git branch"]
        B1[origin-sessions<br/>transcripts / prompts / diffs]
    end
    C[Git commit] --> N1
    C --> B1
```

The metadata can connect a Git commit to an Origin session and its associated AI activity.

This is a major part of Origin's local-first design: AI provenance can live alongside the
repository's Git history.

## Prompt-to-Commit Attribution

Origin attempts to associate AI prompts and sessions with the changes produced by that activity.

A simplified flow is:

```mermaid
flowchart TD
    A[Prompt] --> B[AI agent session]
    B --> C[Agent edits files]
    C --> D[Origin captures Git state]
    D --> E[Commit / snapshot]
    E --> F[Origin metadata]
    F --> G[Code attribution]
```

Origin maintains per-prompt/shadow Git state to distinguish changes made during different
prompts, including changes that occur before a final commit.

## Line-Level Attribution

One of Origin's most notable features is AI-aware Git blame.

Traditional Git can identify the commit responsible for a line:

```bash
git blame src/auth.ts
```

Origin extends this concept:

```mermaid
flowchart TD
    A[Source line] --> B[git blame]
    B --> C[Commit SHA]
    C --> D[Origin Git metadata]
    D --> E[Origin session]
    E --> F[AI agent / model / prompt]
```

This enables commands such as:

```bash
origin blame
origin why
```

to provide AI provenance for source-code changes. See the
[`origin why` announcement](https://getorigin.io/blog/origin-why-line-level-prompt-attribution).

The goal is to answer:

- Which AI agent produced this line?
- Which session produced it?
- Which prompt caused the change?
- Which model was involved?
- Which commit introduced it?

## Tool and File Tracking

Origin recognizes common AI-agent operations for reading and modifying files.

Examples include tools corresponding to:

```text
Read
Write
Edit
NotebookEdit
read_file
write_file
replace
apply_diff
search_replace
```

This allows Origin to build records such as:

```mermaid
flowchart LR
    P[Prompt] --> R[files read]
    P --> M[files modified]
    P --> T[tools invoked]
    P --> D[resulting diff]
```

## Token and Cost Tracking

Origin can track model usage information including:

- input tokens
- output tokens
- cached tokens
- total tokens
- estimated cost

This supports AI cost attribution and usage analysis.

## Local / Solo Architecture

Origin's local mode is designed to work without requiring a centralized Origin server.

Conceptually:

```mermaid
flowchart TD
    Dev[Developer machine] --> CLI[Origin CLI]
    CLI --> AD[Agent data]
    CLI --> GM[Git metadata]
    AD --> LH[Local AI history]
    GM --> LH
```

Origin's public CLI repository documents Git notes and Git refs as part of its local storage
architecture.

This means AI development history can remain local and can be synchronized with the repository
when the relevant Git refs are shared.

## Team / Connected Architecture

Origin also provides a centralized team-oriented product.

The team model adds capabilities such as:

- centralized AI activity visibility
- team dashboards
- user-level AI usage
- cost attribution
- policies
- budgets
- audit information
- governance
- PR checks and automation

Conceptually:

```mermaid
flowchart LR
    A[Developer A] --> P[Origin platform]
    B[Developer B] --> P
    C[Developer C] --> P
    P --> S[Sessions]
    P --> C2[Costs]
    P --> G[Governance]
```

Thus:

```mermaid
flowchart LR
    Solo[Solo] --> L[local-first AI provenance]
    Team[Team / Enterprise] --> C[centralized AI governance and visibility]
```

## Origin vs LLM Gateway Telemetry

Origin should not be confused with conventional LLM API observability.

An LLM gateway such as Envoy AI Gateway can observe:

```mermaid
flowchart LR
    G[LLM gateway] --> A[HTTP request]
    G --> B[user]
    G --> C[model]
    G --> D[tokens]
    G --> E[latency]
    G --> F[status]
    G --> H[backend]
    G --> I[trace]
```

Origin operates closer to the developer and AI-agent layer:

```mermaid
flowchart TD
    A[Developer] --> B[IDE / AI agent]
    B --> C[Prompt]
    C --> D[Tools]
    D --> E[Files]
    E --> F[Git]
```

Therefore they are complementary.

### Gateway telemetry

Answers:

> What happened to the LLM request?

### Agent telemetry

Answers:

> What was the developer trying to accomplish and what did the AI agent do?

### Git provenance

Answers:

> What code actually entered the repository?

A complete enterprise AI engineering telemetry system can combine all three.

## Relevance to camer-digital

For an enterprise AI platform such as `camer-digital`, Origin provides a useful architectural
reference.

A combined architecture could look like:

```mermaid
flowchart TD
    Dev[Developer] --> IDE[IDE / AI Agent]
    IDE --> AT[Agent telemetry]
    IDE --> G[Git]
    G --> C[commits / PRs]
    AT --> CD[camer-digital]
    CD --> EG[Envoy AI Gateway]
    EG --> V[vLLM]
    EG --> O[OTEL]
    V --> GPU[GPU]
    O --> Obs[Observability]
```

Useful correlation identifiers include:

```text
user_id
session_id
conversation_id
turn_id
agent_id
model
repository
branch
commit_sha
trace_id
```

A shared `turn_id` can connect a developer's prompt to the LLM request observed by the gateway.

This produces an end-to-end lineage:

```mermaid
flowchart TD
    A[User] --> B[Prompt]
    B --> C[AI Agent]
    C --> D[LLM request]
    D --> E[Model]
    E --> F[Tokens / latency / GPU]
    F --> G[Tool calls]
    G --> H[Files]
    H --> I[Git commit]
    I --> J[Pull request]
    J --> K[Deployment]
```

## Key Technologies

At a high level, Origin uses:

- **AI-agent hooks** — receive lifecycle and interaction events.
- **Transcript parsing** — extract prompts, tools, models, tokens, and file activity.
- **Agent adapters** — normalize agent-specific formats.
- **Git** — source of truth for repository state, commits, and diffs.
- **Git notes / refs** — associate AI provenance with Git history.
- **Git blame** — trace source lines back to commits and AI metadata.
- **Background session watching** — monitor active sessions and reconcile transcript changes.
- **Centralized platform services** — provide team dashboards, governance, budgets, audit, and
  organizational visibility.

## Summary

Origin can be understood as an **AI-native layer on top of Git and AI coding agents**.

```mermaid
flowchart TD
    A[AI AGENT] -->|prompts / tools / sessions| B[ORIGIN CLI]
    B --> C[AGENT DATA]
    B --> D[GIT]
    D --> E[commits / diffs]
    C --> F[ATTRIBUTION]
    E --> F
    F --> G[prompt → code lineage]
```

The key innovation is not merely collecting LLM telemetry. It is **connecting AI-agent activity
to the actual Git history of the software being produced**.

For an enterprise AI platform, the main architectural lesson is:

> **Combine agent-side telemetry, LLM gateway telemetry, and Git provenance using shared
> session/turn identifiers.**

That provides a much more complete view of AI-assisted software engineering than any single
telemetry layer.
