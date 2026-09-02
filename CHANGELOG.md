# Changelog

## [1.0.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.6.0...v1.0.0) (2026-09-02)


### ⚠ BREAKING CHANGES

* **governance-auth:** scope the command tree, and add a forced refresh ([#261](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/261))

### Features

* **governance-auth:** copilot-push reclaims the spool once it is caught up ([#257](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/257)) ([c5f9da2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c5f9da257ba3702b5d0e172bc00b323fe8e5e18a))
* **governance-auth:** scope the command tree, and add a forced refresh ([#261](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/261)) ([cb562fa](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cb562fac71cf53bfb43c1b48f481a4962b74d0cd))


### Bug Fixes

* **release:** cutting a release broke `curl | sh` until the assets built ([#256](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/256)) ([4706976](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4706976b76c788175d13e938d80590a43bb920de))


### Documentation

* **adr:** a local collector daemon replaces per-client OTLP credentials ([#258](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/258)) ([1d1c667](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1d1c667ddd72c602b3c55e7777116319a83a5abe))

## [0.6.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.5.0...v0.6.0) (2026-09-02)


### Features

* **governance-auth:** log to a rotating file, not just to stderr ([#250](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/250)) ([7184f62](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/7184f62389a975aba372eca07ddd92ba57733ef0))
* **governance-auth:** serve the callback page built in converse-frontends ([#251](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/251)) ([92bd690](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/92bd690b6e1d65d6214160e6af06b02ffc3ca6bc))
* **install:** publish install.sh and uninstall.sh to GitHub Pages ([#248](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/248)) ([4abe03b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4abe03baecda597b397a4d712a58ffe077b515a7))


### Bug Fixes

* **ci:** Pages cannot self-enable; say so instead of failing obscurely ([#255](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/255)) ([8d1415c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8d1415c550ec592b324efe4d33786b18f69c9162))
* **ci:** the Pages deploy failed, so the documented installer 404s ([#254](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/254)) ([3d1139b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3d1139ba6b7a54ae2d2bf9b4f6004e8d3c11b90d)), closes [#225](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/225)
* **vscode:** tell the developer when governance-auth is signed out ([#249](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/249)) ([726dd00](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/726dd00cff8a536c9c01525f7870db5efa5a9eb3))

## [0.5.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.4.0...v0.5.0) (2026-09-02)


### Features

* **governance-auth:** configure wires Copilot otel → file → upload by default ([#247](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/247)) ([8a0a360](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8a0a360d2f7ce7cb8cfbd1637a47646dbe9f23c9))
* **governance-auth:** drain the Copilot OTel spool to the collector ([#228](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/228)) ([6602d2f](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6602d2fd86019d138c16ca9ad06b5761245d9384))
* **governance-auth:** status shows whether telemetry will actually export ([#217](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/217)) ([98d9dfa](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/98d9dfa5d39bccdff790747ce25ece76a414f316))
* **vscode:** a governed LanguageModelChatProvider extension, verified live ([#215](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/215)) ([1b07a53](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1b07a53c0246169ea3c9d2667cfd44eb62589a78))


### Bug Fixes

* **ci:** governance-check reads the PR body at run time, not from the frozen payload ([#220](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/220)) ([ad5e764](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ad5e764eedebe8a1de71febe7f4804c87c00f23e)), closes [#219](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/219)
* **governance-auth:** first run was a dead end ([#214](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/214)) ([fc3dad1](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fc3dad10aab2a16deb4c44798cf5593b0ad9d03b))


### Documentation

* a default flow for the three tools, and upgrade the RFC-0003 matrix ([#218](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/218)) ([2fa5a72](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2fa5a72ddbcf45c3d3e4571d3b6b6e7e2ca5164b))

## [0.4.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.3.1...v0.4.0) (2026-08-31)


### Features

* **chart:** aiCliOtel accepts the exchanged token's audience ([#142](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/142)) ([64b709d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/64b709df32d036834bbad4fdc2922aa3ce64b7df)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance-auth:** bind a registered loopback port, not an ephemeral one ([#203](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/203)) ([ce71c52](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ce71c52484deee5143746bdd4e3bb4f3a5fe438f))
* **governance-auth:** make our provider Codex's default ([#208](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/208)) ([35f5173](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/35f517348c5573aaa8f20c3789b45a716d708b5f)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance-auth:** remember settings, export them to the shell, style the callback page ([#206](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/206)) ([ea244f7](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ea244f799db68fa951a273ac634e006e2a08138d))
* **governance-auth:** status shows a dashboard when a human is looking ([#211](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/211)) ([cb4598a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cb4598a9b703f3becdb273445dc6ce6d3d2e568e))


### Bug Fixes

* **deps:** bump chacha20 0.10.1 -&gt; 0.10.2, unyanked ([#200](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/200)) ([6d8d9a5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6d8d9a56191eab66b0343772a4ad5a220a521e0e))
* **governance-auth:** an expired session read as 'needs refresh, -8338s' ([#213](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/213)) ([4b7d6db](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4b7d6db68037ac8018975ae44ff7429c2b59b0dc))
* **governance-auth:** stop an empty lock file blocking token for 300s ([#154](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/154)) ([b2cb1b2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b2cb1b280a044e6864eb28d6bd4f75642cda266e))
* **governance-auth:** the callback page said 'signed in' when sign-in failed ([#204](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/204)) ([b663d26](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b663d26760a56d4b4aac280f04a1bb77fdf9a8a8)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance:** alert on a failed copilot-verify CronJob run ([711c4a2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/711c4a28e033ec35b2419bb6781980600ce42cbb))
* **governance:** alert on a failed copilot-verify CronJob run ([#181](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/181)) ([0753edb](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/0753edbffec7dd7215d71d0414a6287bb176fc87))
* **loc-gate:** measure renamed files instead of skipping them. ([66e241b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/66e241b5f3bb7d1e4f685f3bfb7cf2267ac825d6))


### Code Refactoring

* **governance-auth:** move the callback page into a minijinja template ([#209](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/209)) ([86f2fbf](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/86f2fbfd5ef3dde2a6d82dae2174d9cfb89653a1))


### Continuous Integration

* add 200 LoC gate for new and changed rust files. ([45bd4e4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/45bd4e415782e1fa8b2e8ea9d2e807f05531bf35))
* add 200-LoC gate for new and changed Rust files ([d06ce26](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d06ce2656412b874e89c0d713f42cca1733207be))


### Documentation

* **adr:** ADR-0013 is Accepted — ratified with ADR-0014's adoption ([#207](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/207)) ([8519ba4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8519ba437b6e595428dbf4793f8cc2be804b4939))
* **adr:** ADR-0014 — usage telemetry consolidates into the authz usage store ([#198](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/198)) ([d7650cf](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d7650cf0ae9d65ce46e07ea41fd98cc195a339e8)), closes [#182](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/182)
* **governance-auth:** add the reference manual ([#155](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/155)) ([3f510fd](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3f510fde0c41b5797df05d6266fa526ac5c1771d))
* **governance-auth:** correct the issuer, the stated blocker, and a superseded claim ([#199](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/199)) ([d552b43](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d552b432fe7b245b7bc9499f879e7313d27aa47c))
* **governance-auth:** record that logout is not immediate cutoff ([#156](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/156)) ([4268406](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4268406b0a0b2dc12fc6b73bb71670adb318058f))
* **governance-auth:** record the pinned-port decision and bring the manual current ([#205](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/205)) ([15823b5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/15823b5e8977c9f66da25fc3864d42586fe7dd0a)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance-auth:** retire token exchange, make --device-code the login ([#202](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/202)) ([80f87ef](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/80f87ef8d81916e537763ba91c984774c0cfab1c)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **rfc:** RFC-0003 telemetry source taxonomy, and ADR-0013 ingest invariants ([#157](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/157)) ([a171f8c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a171f8c62de5e83a080df7aa092e001287a0c29c))

## [0.3.1](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.3.0...v0.3.1) (2026-08-16)


### Bug Fixes

* **governance-auth:** three defects found by running v0.3.0 in production ([#148](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/148)) ([bec7403](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/bec7403fe3742821a70aa0fbb6c35733309499bd)), closes [#145](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/145) [#146](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/146) [#147](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/147)

## [0.3.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.2.1...v0.3.0) (2026-08-16)


### Features

* **governance-auth:** config file layering (ADR-0012 Decision 2) ([#138](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/138)) ([b9abe0e](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b9abe0e145fc5347fc9a7230a3632f90bdf6cfb7))
* **governance-auth:** provider-agnostic config, optional token exchange, no auto-browser ([#143](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/143)) ([89fa707](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/89fa70702d5c7f77ff6946bcdd31eb294830cfe7))


### Bug Fixes

* **governance-auth:** decouple inference wiring from the telemetry endpoint ([#137](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/137)) ([830b890](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/830b890c6a979700de49d39e8ed9ca6366c713a7))

## [0.2.1](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.2.0...v0.2.1) (2026-08-16)


### Bug Fixes

* **governance-auth:** report the release version, ending the self-update loop ([#135](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/135)) ([83d0846](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/83d0846a9a598055388b7b685d7ce8d641dee242)), closes [#134](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/134)

## [0.2.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v0.1.0...v0.2.0) (2026-08-14)


### Features

* **chart:** dedicated OTel collector for copilot-sync, and fix its metrics to be gauges ([#67](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/67)) ([cd8f3c9](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cd8f3c91bfdb04f93f310d7ee40b88b307538150))
* **chart:** PrometheusRule for the alert-grade metric families ([#73](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/73)) ([ac2a6bc](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ac2a6bc8b011cc0b3f77360b2d0635006dc74c66))
* **chart:** schedule the copilot-verify reconciliation CronJob ([#93](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/93)) ([4ab704a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4ab704a372e4b5e9e0b6b013baac7e0aec714cbc))
* **copilot:** ingest Copilot seat snapshots (RFC-0001's headline use case) ([#70](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/70)) ([2edf614](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2edf614b6a7104456f8d467b0290dcd00a301c80))
* **dashboards:** Copilot connector dashboard, generated by script and shipped as a GrafanaDashboard CR ([#68](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/68)) ([532e502](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/532e502328068118f1651588f815d6bac26f973a))
* **governance-auth,chart:** OTEL export for AI clients + public OTLP collector ([#83](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/83)) ([f381d84](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f381d84b6e1b51bc83d49caba8bdee17b88a16f6))
* **governance-auth:** write inference config, with absolute command paths ([#91](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/91)) ([1542873](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/15428737c32196e82b5488baf006631bef7ffafb))
* **metrics:** org-level KPI gauges, alert-grade and derived from Postgres ([#72](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/72)) ([9965c1a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/9965c1ae24a8078df67176130365d6e023e8341b))


### Bug Fixes

* **chart:** drop the ai-cli-otel egress restriction entirely -- live-proven this time ([#90](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/90)) ([750166d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/750166d4957ce8003280625f9516f6462e127323))
* **chart:** make the Copilot dashboard actually reconcile, and nest it under Governance ([#69](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/69)) ([b769c8b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b769c8bacf19893a7911d560c1599ab179f9f43d))
* **chart:** pin the wait-for-oidc-issuer init container's UID/GID explicitly ([#87](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/87)) ([c92b46a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c92b46ae0da154d23c749a313d29cebedfe3dea1))
* **chart:** replace the broken toFQDNs egress rule with toCIDR ([#88](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/88)) ([76cf4dd](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/76cf4dda75c16e72da463cb75c83c98336c4b9ba))
* **chart:** retry OIDC discovery in an init container before the ai-cli-otel collector starts ([#85](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/85)) ([fa7bc4d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fa7bc4d93b680b46fd84338befef125ecb8819e9))
* **chart:** route the collector's OIDC issuer through Traefik's ClusterIP, not external egress ([#89](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/89)) ([b28b39c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b28b39ca532c3804e56ab0c31a92ea437efee472))
* **ci:** ship musl Linux assets and stop building on a retired macOS runner ([#131](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/131)) ([4a0176a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4a0176abf2d8596d6317291ca62e5e9125d24ca9))
* **copilot:** stop the backfill window including today, which GitHub never has ([#77](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/77)) ([4dc0e75](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4dc0e750b960ba30c24ba37b7846d9b69addeabf))
* **governance-auth:** move the session out of the cache, and stop self-update fighting package managers ([#129](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/129)) ([b90b6ef](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b90b6ef0166088ac773691b59e33ca501427c9b4))
* **governance-auth:** PKCE on the device-code flow, and CLI arg-order independence ([#82](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/82)) ([f80914d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f80914d89aed2ba0f0aaaad951eb75276c2fe83e))
* **redact-extproc:** decompress gzip response bodies; strip Accept-Encoding upstream; buffer+scan SSE+gzip ([#86](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/86)) ([01f6be5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/01f6be5279a3e8efd8096038e34e7ff3b50b9543))
* **redact-extproc:** remove Content-Length header on body mutation for Envoy v1.32 ([#74](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/74)) ([cd18c5b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cd18c5bee968b836258150eb64eea50eea9cfb46))
* **tenant:** refuse an empty TENANT_ID, and add real seat-hygiene panels ([#71](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/71)) ([6140e55](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6140e5519ee4995dad6afa54c00cc89f68c508e8))


### Code Refactoring

* **redact-extproc:** remove Accept-Encoding stripping and debug logs ([#94](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/94)) ([fea01eb](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fea01eb990495dc628bebd123cccf54214766449))


### Continuous Integration

* run release-please, and chain the binary asset build to it ([#132](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/132)) ([047ea92](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/047ea926c32c7a1775e48d84dadd52f2aa4a97f0))


### Documentation

* **adr:** ADR-0012, governance-auth packaging and distribution ([#130](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/130)) ([42802c0](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/42802c06a943bb80a0a92949c678c4f8e516b0c7))
* **agents:** add house CUID2 identifier rule (ADR-0039) ([#79](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/79)) ([3cc897b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3cc897b146da51b9b978092d55385be8abeea658))
* **agents:** correct the persistence-layer rule to match what the code does ([#80](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/80)) ([8744764](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8744764a7fb45c998cb7d4ecd9af6f372f217ef4))
* **foundry:** correct the README, which still described a scaffold ([#76](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/76)) ([e42fe80](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/e42fe800b5ccd622f20b4765daea8291a577ab19))
* **integrations:** sequence diagrams for every support-matrix row ([#128](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/128)) ([de05b7c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/de05b7c7f9836a5eb0c71608d8588da116a2821b))
* **runbook:** document Content-Length root cause, fix verification, and production config ([62d5ac3](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/62d5ac3262fc74de6d70811d41a02af17d7369ba))
