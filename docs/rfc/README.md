# RFCs

An RFC specifies **what we are going to build** and is revised until it is agreed. An
[ADR](../adr/README.md) records **a decision and its consequences** and then freezes.

An RFC usually produces several ADRs. RFC-0001 produced ADR-0002 (Postgres over Parquet)
and ADR-0007 (where the metrics come from); RFC-0002 produced ADR-0004 (tenancy) and
ADR-0006 (auth). The ADRs are the durable record; the RFC is the working document.

## Index

| # | Title | Status |
|---|---|---|
| [0001](./0001-github-copilot-connector.md) | GitHub Copilot connector | Draft |
| [0002](./0002-microsoft-foundry-otlp-ingestion.md) | Microsoft Foundry OTLP ingestion | Draft |

Statuses: `Draft` -> `In review` -> `Accepted` -> `Implemented` | `Withdrawn`.

## Writing one

Copy `template.md`. Keep the **Open questions** section alive -- an RFC with no open
questions has either been fully agreed or has not been read carefully.
