# Changelog

## [2.3.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v2.2.1...v2.3.0) (2026-09-04)


### Features

* **governance-auth:** configure --profile daemon|manual ([#280](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/280)) ([0969eae](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/0969eaebc5de32f7daccfab569e0f6f6547d61bd))
* **governance-auth:** pin OTEL loopback port and client URL (contract) ([c66e4fd](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c66e4fd8f7f4cbae541fe54e136772144e10ec69))
* **governance-auth:** serve otel loopback collector daemon (issue [#268](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/268)) ([#290](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/290)) ([2d2c155](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2d2c155c3a5dbe08854ff19b4619eea127c6e409))
* **governance-auth:** status carries a daemon row ([#271](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/271)) ([#295](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/295)) ([f22a107](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f22a107d220e2f284de00e56270135602b01b084))


### Bug Fixes

* **governance-auth:** address otel_port review - fail-closed, drop baseline, derive URL. ([0b00525](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/0b00525605843c511da09b9ffd75721084e23a19))
* **governance-auth:** telemetry row is daemon-profile-blind (found in live E2E) ([#296](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/296)) ([6e8f863](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6e8f863615f6dc6e4aa208f05d900904c338c350))

## [2.2.1](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v2.2.0...v2.2.1) (2026-09-02)


### Bug Fixes

* **governance-auth:** a printed token must outlive the caller's cache window ([#287](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/287)) ([703f3dd](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/703f3dd468a58bf957f7ff6e58b5f2951a80503e))

## [2.2.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v2.1.0...v2.2.0) (2026-09-02)


### Features

* **charts:** second public OTLP collector for OpenCode laptops ([#283](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/283)) ([381275c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/381275cd47b2c99c051957b65c0011b10f35f8a0))


### Bug Fixes

* **ci:** chart-checks builds helm dependencies before rendering ([#170](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/170)) ([3fee4bc](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3fee4bc01a7c2ed2a3aa95e393f3c7052a5377c1))
* **governance-auth:** stop exporting the OTLP endpoint machine-wide ([#286](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/286)) ([416e4c3](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/416e4c3a255bff05c10cce65e849bb9deff17dac))


### Documentation

* **integrations:** correct opencode's issuer and telemetry status in the support matrix ([#285](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/285)) ([7ee6e74](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/7ee6e74721860507691adf5b86e9f3118138ed25))

## [2.1.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v2.0.0...v2.1.0) (2026-09-02)


### Features

* **governance-auth:** --no-claude/--no-codex/--no-vscode leave a client alone ([#274](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/274)) ([95328d8](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/95328d8250fa9a3f9dbf330743f2f74acf2ae37c))
* **install:** fall back to the newest release that has the asset ([#267](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/267)) ([de741d9](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/de741d9409ac43edbecbc711681b84c5ee9bc712)), closes [#265](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/265)


### Reverts

* **release:** drop the draft-release flow, and record why ([#266](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/266)) ([3c3b5d1](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3c3b5d179ed3edd6fc70caf631ea498325a970ae))


### Code Refactoring

* **governance-auth:** flag help is for users, rationale is for docs ([#273](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/273)) ([a3571f9](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a3571f9d7f91ca44dd9ab526d6751e921e582211))


### Documentation

* **governance-auth:** retire the "authz serves no /authorize" claim ([#168](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/168)) ([#195](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/195)) ([14eca94](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/14eca94932031275fd30c970e23509e858f219c4))

## [2.0.0](https://github.com/ADORSYS-GIS/lightbridge-governance/compare/v1.0.0...v2.0.0) (2026-09-02)


### ⚠ BREAKING CHANGES

* **governance-auth:** scope the command tree, and add a forced refresh ([#261](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/261))
* the hand-written sqlx migrations are removed. The schema at crates/governance-core/schema/governance.cstack is now the source of truth for tables, migrations, CRUD and routes.

### Features

* /internal/v1/resolve for Authorino, fail-closed and TTL-cacheable ([#11](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/11)) ([#25](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/25)) ([1ef96cb](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1ef96cb3eb3da1909cbed44fbc0772f2fde5cb6f))
* **auth:** add governance-auth, a Keycloak OAuth2 credential helper for Claude Code/Codex ([#55](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/55)) ([fb5a7c2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fb5a7c2bddb1244b20e004b012414bc74ded8449))
* **chart:** add lightbridge-governance Helm templates ([#42](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/42)) ([#43](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/43)) ([34f83f2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/34f83f2a3c6f8f6bde50f62f0b66c527d260080f))
* **chart:** aiCliOtel accepts the exchanged token's audience ([#142](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/142)) ([64b709d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/64b709df32d036834bbad4fdc2922aa3ce64b7df)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **chart:** dedicated OTel collector for copilot-sync, and fix its metrics to be gauges ([#67](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/67)) ([cd8f3c9](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cd8f3c91bfdb04f93f310d7ee40b88b307538150))
* **chart:** PrometheusRule for the alert-grade metric families ([#73](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/73)) ([ac2a6bc](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ac2a6bc8b011cc0b3f77360b2d0635006dc74c66))
* **chart:** schedule the copilot-verify reconciliation CronJob ([#93](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/93)) ([4ab704a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4ab704a372e4b5e9e0b6b013baac7e0aec714cbc))
* complete identity binding with mismatch detection ([#35](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/35)) ([29979db](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/29979dbea3eb7901dc01544e8f0fb9ab1e050b5d))
* complete mismatch detection for identity binding ([#35](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/35)) ([788cf06](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/788cf06a22cc37ea3026f371aa53fcc412e520e4))
* **copilot:** ingest Copilot seat snapshots (RFC-0001's headline use case) ([#70](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/70)) ([2edf614](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2edf614b6a7104456f8d467b0290dcd00a301c80))
* **dashboards:** Copilot connector dashboard, generated by script and shipped as a GrafanaDashboard CR ([#68](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/68)) ([532e502](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/532e502328068118f1651588f815d6bac26f973a))
* enforce tenant FK on Integration/IdentityMap, prove upsert idempotency ([#16](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/16), [#17](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/17)) ([#22](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/22)) ([fb7482c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fb7482c508c2a3c207082ab84c96825c04cf93bd))
* **governance-auth,chart:** OTEL export for AI clients + public OTLP collector ([#83](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/83)) ([f381d84](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f381d84b6e1b51bc83d49caba8bdee17b88a16f6))
* **governance-auth:** bind a registered loopback port, not an ephemeral one ([#203](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/203)) ([ce71c52](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ce71c52484deee5143746bdd4e3bb4f3a5fe438f))
* **governance-auth:** config file layering (ADR-0012 Decision 2) ([#138](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/138)) ([b9abe0e](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b9abe0e145fc5347fc9a7230a3632f90bdf6cfb7))
* **governance-auth:** configure wires Copilot otel → file → upload by default ([#247](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/247)) ([8a0a360](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8a0a360d2f7ce7cb8cfbd1637a47646dbe9f23c9))
* **governance-auth:** copilot-push reclaims the spool once it is caught up ([#257](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/257)) ([c5f9da2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c5f9da257ba3702b5d0e172bc00b323fe8e5e18a))
* **governance-auth:** drain the Copilot OTel spool to the collector ([#228](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/228)) ([6602d2f](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6602d2fd86019d138c16ca9ad06b5761245d9384))
* **governance-auth:** log to a rotating file, not just to stderr ([#250](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/250)) ([7184f62](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/7184f62389a975aba372eca07ddd92ba57733ef0))
* **governance-auth:** make our provider Codex's default ([#208](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/208)) ([35f5173](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/35f517348c5573aaa8f20c3789b45a716d708b5f)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance-auth:** provider-agnostic config, optional token exchange, no auto-browser ([#143](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/143)) ([89fa707](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/89fa70702d5c7f77ff6946bcdd31eb294830cfe7))
* **governance-auth:** remember settings, export them to the shell, style the callback page ([#206](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/206)) ([ea244f7](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ea244f799db68fa951a273ac634e006e2a08138d))
* **governance-auth:** scope the command tree, and add a forced refresh ([#261](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/261)) ([cb562fa](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cb562fac71cf53bfb43c1b48f481a4962b74d0cd))
* **governance-auth:** serve the callback page built in converse-frontends ([#251](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/251)) ([92bd690](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/92bd690b6e1d65d6214160e6af06b02ffc3ca6bc))
* **governance-auth:** status shows a dashboard when a human is looking ([#211](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/211)) ([cb4598a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cb4598a9b703f3becdb273445dc6ce6d3d2e568e))
* **governance-auth:** status shows whether telemetry will actually export ([#217](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/217)) ([98d9dfa](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/98d9dfa5d39bccdff790747ce25ece76a414f316))
* **governance-auth:** write inference config, with absolute command paths ([#91](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/91)) ([1542873](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/15428737c32196e82b5488baf006631bef7ffafb))
* **governance-copilot:** Copilot collector fetch, archive, upsert, self-healing backfill  ([#54](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/54)) ([fc0cc80](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fc0cc80db8a87fa1bfe35d4be7c45972473b8f9b))
* **identity:** add verify attribution and identity directory sync ([#35](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/35)) ([#66](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/66)) ([de7fe57](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/de7fe57dc5a4021c7d17c5b526ae74058da4bb91))
* implement mismatch detection for identity binding ([#35](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/35)) ([3a7360b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3a7360b6584b36dd915a0548c41b0d998abe29cb))
* implement per-developer identity binding for telemetry attribution ([#35](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/35)) ([c13aa55](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c13aa55483f92eb6edaa167a3f2c6c6628a11185))
* **ingest:** complete story [#31](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/31) acceptance criteria ([d6a4be7](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d6a4be7e45b5745e3e4a68974e1b865a30e79525))
* **ingest:** generalise OTLP ingest into a provider-agnostic push co… ([fd63d3c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fd63d3cbcfdc91013a3c6a33a8f394d7142184d8))
* **ingest:** generalise OTLP ingest into a provider-agnostic push connector ([#31](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/31)) ([323d3c9](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/323d3c9b1b15b76e275be01e9dc08445a7fbb44b))
* **install:** publish install.sh and uninstall.sh to GitHub Pages ([#248](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/248)) ([4abe03b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4abe03baecda597b397a4d712a58ffe077b515a7))
* integration credential issuance, revocation and resolution ([#10](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/10)) ([#24](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/24)) ([5539889](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/5539889d187865b6e024dc703f3d7f462a9a92a6))
* **metrics:** org-level KPI gauges, alert-grade and derived from Postgres ([#72](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/72)) ([9965c1a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/9965c1ae24a8078df67176130365d6e023e8341b))
* model environments in registry ([#14](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/14)) ([c930cf7](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c930cf7fabbd899d1a05f3ccc0f1317ad439a592))
* **redact-extproc:** ext_proc engine, server, and Docker image (ADR-0116) ([#44](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/44)) ([54718b2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/54718b2f5513f4a7faa04d452b42fec1e1a54553))
* **redact-gateway:** implement incremental SSE streaming with SseHoldBack ([b8a7b8d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b8a7b8db20c2b6c3b52beab9b42d7728655528bd))
* **redact:** buffered SSE stream redaction ([3d58dc8](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3d58dc8477ba375396cb2ffba92b7d53ade16925))
* **redact:** chart and image for the redaction proxy ([99b2032](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/99b203277130f10659106fa93d159b1c8599448d))
* **redact:** compose stack for testing redact-gateway against a real LLM ([59cac79](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/59cac79b3c15603651cfd8dfb804f2d949f5652e))
* **redact:** compose stack for testing redact-gateway against a real LLM ([b0c503e](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b0c503e7ebdacc20f8dae61ba6d56fe1bf5b3a86))
* **redact:** first-party PII redaction engine, replacing censgate ([c1a56c2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c1a56c23fd9c13f408e02e16835fa868ea17f47e))
* **redact:** OpenAI-compatible redaction proxy ([ac94d32](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ac94d32a0635ed61017fc9263da718c2c8b5eb53))
* **redact:** scan OpenAI-shaped request and response bodies ([fca4e88](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fca4e889f42032b2372ba17088644c5a8b5f1517))
* run migrations end-to-end and mount the registry router ([#18](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/18)) ([b2cb5c3](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b2cb5c3884dc38dcf4d1c594d3d3b463c01c198b))
* run migrations end-to-end and mount the registry router ([#18](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/18)) ([438db1c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/438db1c6c23793aaeacb956dffe44ac30352264c))
* scaffold the lightbridge-governance platform ([8be0034](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8be003490df7bf26321bf283021630b39d2db969))
* **vscode:** a governed LanguageModelChatProvider extension, verified live ([#215](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/215)) ([1b07a53](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1b07a53c0246169ea3c9d2667cfd44eb62589a78))


### Bug Fixes

* boot failure, resolve cache, OAuth TLS, redaction leak paths, charts on app-template v4 ([#56](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/56)) ([2898dde](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2898dde86d45043ebc21bc1a1fe6f74f2ded45eb))
* **chart:** drop the ai-cli-otel egress restriction entirely -- live-proven this time ([#90](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/90)) ([750166d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/750166d4957ce8003280625f9516f6462e127323))
* **chart:** make the Copilot dashboard actually reconcile, and nest it under Governance ([#69](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/69)) ([b769c8b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b769c8bacf19893a7911d560c1599ab179f9f43d))
* **chart:** pin the wait-for-oidc-issuer init container's UID/GID explicitly ([#87](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/87)) ([c92b46a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c92b46ae0da154d23c749a313d29cebedfe3dea1))
* **chart:** replace the broken toFQDNs egress rule with toCIDR ([#88](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/88)) ([76cf4dd](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/76cf4dda75c16e72da463cb75c83c98336c4b9ba))
* **chart:** retry OIDC discovery in an init container before the ai-cli-otel collector starts ([#85](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/85)) ([fa7bc4d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fa7bc4d93b680b46fd84338befef125ecb8819e9))
* **chart:** route the collector's OIDC issuer through Traefik's ClusterIP, not external egress ([#89](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/89)) ([b28b39c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b28b39ca532c3804e56ab0c31a92ea437efee472))
* **chart:** wire INTERNAL_INGEST_TOKEN into the governance Deployment ([#57](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/57)) ([37ae024](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/37ae02487cc415cf976396cae1c9bd86efd775f3))
* **ci:** governance-check reads the PR body at run time, not from the frozen payload ([#220](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/220)) ([ad5e764](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ad5e764eedebe8a1de71febe7f4804c87c00f23e)), closes [#219](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/219)
* **ci:** Pages cannot self-enable; say so instead of failing obscurely ([#255](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/255)) ([8d1415c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8d1415c550ec592b324efe4d33786b18f69c9162))
* **ci:** ship musl Linux assets and stop building on a retired macOS runner ([#131](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/131)) ([4a0176a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4a0176abf2d8596d6317291ca62e5e9125d24ca9))
* **ci:** the Pages deploy failed, so the documented installer 404s ([#254](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/254)) ([3d1139b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3d1139ba6b7a54ae2d2bf9b4f6004e8d3c11b90d)), closes [#225](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/225)
* **codex:** address critical review issues ([469ec8c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/469ec8cc101f3fdd28637c2a2f87c953c7a3a79b))
* **codex:** handle exec mode token count attributes ([91218e5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/91218e5f20a1fe1cf85d91bbc690c0f1dd17478d))
* **copilot:** stop the backfill window including today, which GitHub never has ([#77](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/77)) ([4dc0e75](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4dc0e750b960ba30c24ba37b7846d9b69addeabf))
* **copilot:** stop the collector failing silently, and implement ADR-0007's connector metrics ([#64](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/64)) ([21b0391](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/21b039172b0d2dab538c9e89f5dec3d91249b62e))
* **deps:** add tokio to governance-core dependencies ([a7aa44a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a7aa44ac6098a3e548d861fe673f941018adb2c3))
* **deps:** bump chacha20 0.10.1 -&gt; 0.10.2, unyanked ([#200](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/200)) ([6d8d9a5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6d8d9a56191eab66b0343772a4ad5a220a521e0e))
* **deps:** drop the vulnerable legacy TLS stack and fix the supply-chain gate ([14cdedf](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/14cdedfc0e2bd8184d53d675738e80c8a554d94a))
* **docs:** correct story references and remove non-existent metric ([293349f](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/293349fa52a37e20d7a42413ab777fee2fa24695))
* eliminate the migrate_and_create deadlock with a DDL/DML isolation lock ([f2356b0](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f2356b0a7e260f6fe10e3c377c6ac2ee8af5ad31))
* **foundry:** reject malformed events and overflowing timestamps instead of inventing data ([#65](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/65)) ([54a0d1b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/54a0d1b3caa30eaf0cf5c477a99316c5516c1854))
* **governance-auth:** an expired session read as 'needs refresh, -8338s' ([#213](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/213)) ([4b7d6db](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4b7d6db68037ac8018975ae44ff7429c2b59b0dc))
* **governance-auth:** decouple inference wiring from the telemetry endpoint ([#137](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/137)) ([830b890](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/830b890c6a979700de49d39e8ed9ca6366c713a7))
* **governance-auth:** first run was a dead end ([#214](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/214)) ([fc3dad1](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fc3dad10aab2a16deb4c44798cf5593b0ad9d03b))
* **governance-auth:** move the session out of the cache, and stop self-update fighting package managers ([#129](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/129)) ([b90b6ef](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b90b6ef0166088ac773691b59e33ca501427c9b4))
* **governance-auth:** PKCE on the device-code flow, and CLI arg-order independence ([#82](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/82)) ([f80914d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f80914d89aed2ba0f0aaaad951eb75276c2fe83e))
* **governance-auth:** report the release version, ending the self-update loop ([#135](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/135)) ([83d0846](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/83d0846a9a598055388b7b685d7ce8d641dee242)), closes [#134](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/134)
* **governance-auth:** stop an empty lock file blocking token for 300s ([#154](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/154)) ([b2cb1b2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b2cb1b280a044e6864eb28d6bd4f75642cda266e))
* **governance-auth:** the callback page said 'signed in' when sign-in failed ([#204](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/204)) ([b663d26](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b663d26760a56d4b4aac280f04a1bb77fdf9a8a8)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance-auth:** three defects found by running v0.3.0 in production ([#148](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/148)) ([bec7403](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/bec7403fe3742821a70aa0fbb6c35733309499bd)), closes [#145](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/145) [#146](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/146) [#147](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/147)
* **governance-copilot:** accept integer or string id fields in Copilot reports ([#63](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/63)) ([f537552](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/f5375523d1975e3f637ceac20074d0f4ad0a6b3c))
* **governance-copilot:** send a User-Agent so GitHub stops 403 the collector ([#62](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/62)) ([a2401a8](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a2401a81d7d17b3633787993b9f0af91a1eb2640))
* **governance-redact:** stop disabling phone detection in the coding-assistant profile ([#40](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/40)) ([88846ba](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/88846ba9e8527d3f97c7690516d422c1cc7ebf63))
* **governance:** alert on a failed copilot-verify CronJob run ([711c4a2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/711c4a28e033ec35b2419bb6781980600ce42cbb))
* **governance:** alert on a failed copilot-verify CronJob run ([#181](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/181)) ([0753edb](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/0753edbffec7dd7215d71d0414a6287bb176fc87))
* hand-add the unique indexes cratestack silently drops for @[@unique](https://github.com/unique) ([82d7b80](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/82d7b805e759cc3ccc2935457788b0dcb7a83ebf))
* **ingest:** add retry logic for PostgreSQL deadlocks in parallel tests ([574a51c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/574a51ccb2a1cd91b9fc8e37fb87c05ccac255ad))
* **ingest:** address code review issues for story [#33](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/33) ([be22ea4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/be22ea40ab7cce9982d2a84ab5b430a72d3cc5ee))
* **ingest:** use map_or instead of map().unwrap_or() on Result (clippy 1.97) ([1ae2e09](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1ae2e09d80bebe410d28c907d0942d84c532a1ab))
* **loc-gate:** measure renamed files instead of skipping them. ([66e241b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/66e241b5f3bb7d1e4f685f3bfb7cf2267ac825d6))
* make updated_at actually advance on UPDATE ([#21](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/21)) ([#23](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/23)) ([4c0f94d](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4c0f94da7b8c708f3537db3c0081e6415ef879be))
* **redact-extproc:** advance phase on a bodyless request ([#59](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/59)) ([a3da8bf](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a3da8bf642a1477202869e21206d4fcd252e4648))
* **redact-extproc:** decompress gzip response bodies; strip Accept-Encoding upstream; buffer+scan SSE+gzip ([#86](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/86)) ([01f6be5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/01f6be5279a3e8efd8096038e34e7ff3b50b9543))
* **redact-extproc:** remove Content-Length header on body mutation for Envoy v1.32 ([#74](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/74)) ([cd18c5b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cd18c5bee968b836258150eb64eea50eea9cfb46))
* **redact-extproc:** stop failing closed on a chunk-boundary UTF-8 split ([#47](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/47)) ([b3c0a2e](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/b3c0a2e9604dc8da6d34faa47a5a8877a2107c63))
* **redact-gateway:** allow egress to the gateway's POD port, not the Service port ([#39](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/39)) ([2194164](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2194164fac4b9a1365e2e4e9701e2cd63a2180f1))
* **redact-gateway:** render MAX_BODY_BYTES as an integer, not scientific notation ([3b9abe7](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3b9abe7160db766e8342536da51d05f115a01ddb))
* **redact:** cap the upstream response body ([e0ace67](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/e0ace6715884225fd0e99099368a82a83f5d9109))
* **release:** cutting a release broke `curl | sh` until the assets built ([#256](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/256)) ([4706976](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4706976b76c788175d13e938d80590a43bb920de))
* resolve CI test failures for identity binding ([621f97a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/621f97a9f46fc5a0a4b4e3ae26107dc01b046d06))
* **review pass 2:** window size, UTF-8 carry, fail_closed, metrics, ADR and justfile fixes ([29ae332](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/29ae33274af25c3341facff43d7367f30d7fc3a4))
* **review:** deliver real SSE streaming and accurate scanned_fields ([c900cc0](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c900cc092c6c1686bd3a438de7f68f8e0a2f7176))
* **review:** justfile healthcheck curl flags and test-6 token ([267d9bb](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/267d9bb74f79c46d76a76bfcda2671ce2c7d4168))
* **review:** script bugs, doc numbers, and env example ([e14039b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/e14039bd8d353dd4d31def5171067a9e6a37dc15))
* **review:** sse.rs doc says '2 KB' but DEFAULT_WINDOW is 4 KB ([637edf1](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/637edf1c3d9575406ff265c3b53515dc64e90c95))
* satisfy nightly rustfmt import grouping (CI runs +nightly, not stable) ([4fc4dda](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4fc4ddabfc6b31bb13b9ff477d9fb6067e42a976))
* **tenant:** refuse an empty TENANT_ID, and add real seat-hygiene panels ([#71](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/71)) ([6140e55](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6140e5519ee4995dad6afa54c00cc89f68c508e8))
* **test:** add deadlock retry to test fixtures ([0a1b3fc](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/0a1b3fc7c22248db1843a0ad60338426539369b2))
* **vscode:** tell the developer when governance-auth is signed out ([#249](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/249)) ([726dd00](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/726dd00cff8a536c9c01525f7870db5efa5a9eb3))


### Performance Improvements

* **redact:** build one anonymize operator per entity type, not per detection ([5d631e2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/5d631e220a730fd78da9aa34869727f4b9a9b720))


### Code Refactoring

* adopt cratestack as the only persistence layer; REST + CBOR ([0ced954](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/0ced954bab5398f84d70effdb2fbd7d9bb191a37))
* **governance-auth:** move the callback page into a minijinja template ([#209](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/209)) ([86f2fbf](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/86f2fbfd5ef3dde2a6d82dae2174d9cfb89653a1))
* **redact-extproc:** remove Accept-Encoding stripping and debug logs ([#94](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/94)) ([fea01eb](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/fea01eb990495dc628bebd123cccf54214766449))


### Continuous Integration

* add 200 LoC gate for new and changed rust files. ([45bd4e4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/45bd4e415782e1fa8b2e8ea9d2e807f05531bf35))
* add 200-LoC gate for new and changed Rust files ([d06ce26](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d06ce2656412b874e89c0d713f42cca1733207be))
* **docker:** cosign-sign every pushed image digest ([#60](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/60)) ([248a432](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/248a432e1e074208052f7699822c3d5620de1fe4))
* publish charts to OCI, so ai-helm can actually resolve them ([1aadf7a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1aadf7a5f537d43cb29820381fb375b3b572b3c7))
* run release-please, and chain the binary asset build to it ([#132](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/132)) ([047ea92](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/047ea926c32c7a1775e48d84dadd52f2aa4a97f0))


### Build System

* **deps:** declare jsonwebtoken 11 (unconsumed today) ([a563040](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a563040d1f9d10a725fbc6ca06d1cfe3abbae5af))
* **lints:** enforce the Rust rules mechanically, and document the rest ([09e288a](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/09e288a1a894927c7bd5d5c8c92b2370102371cd))


### Documentation

* a default flow for the three tools, and upgrade the RFC-0003 matrix ([#218](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/218)) ([2fa5a72](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/2fa5a72ddbcf45c3d3e4571d3b6b6e7e2ca5164b))
* add READMEs for every crate, app, and chart ([#45](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/45)) ([415d877](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/415d8778f381059a017ccb898927ae9914de23ff))
* **adr:** a local collector daemon replaces per-client OTLP credentials ([#258](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/258)) ([1d1c667](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/1d1c667ddd72c602b3c55e7777116319a83a5abe))
* **adr:** ADR-0012, governance-auth packaging and distribution ([#130](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/130)) ([42802c0](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/42802c06a943bb80a0a92949c678c4f8e516b0c7))
* **adr:** ADR-0013 is Accepted — ratified with ADR-0014's adoption ([#207](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/207)) ([8519ba4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8519ba437b6e595428dbf4793f8cc2be804b4939))
* **adr:** ADR-0014 — usage telemetry consolidates into the authz usage store ([#198](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/198)) ([d7650cf](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d7650cf0ae9d65ce46e07ea41fd98cc195a339e8)), closes [#182](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/182)
* **agents:** add house CUID2 identifier rule (ADR-0039) ([#79](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/79)) ([3cc897b](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3cc897b146da51b9b978092d55385be8abeea658))
* **agents:** correct the persistence-layer rule to match what the code does ([#80](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/80)) ([8744764](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/8744764a7fb45c998cb7d4ecd9af6f372f217ef4))
* **codex:** add comprehensive test plan for telemetry rollout ([ecec1f8](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/ecec1f8dcc8801a8d69d105f78a5ecf31e6c5f29))
* document the prod-profile LTO tradeoff in Cargo.toml ([c34aaa2](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/c34aaa2b7a182c8309e3b736854d4042c4ec1a71))
* **foundry:** correct the README, which still described a scaffold ([#76](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/76)) ([e42fe80](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/e42fe800b5ccd622f20b4765daea8291a577ab19))
* **governance-auth:** add the reference manual ([#155](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/155)) ([3f510fd](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/3f510fde0c41b5797df05d6266fa526ac5c1771d))
* **governance-auth:** correct the issuer, the stated blocker, and a superseded claim ([#199](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/199)) ([d552b43](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d552b432fe7b245b7bc9499f879e7313d27aa47c))
* **governance-auth:** record that logout is not immediate cutoff ([#156](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/156)) ([4268406](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4268406b0a0b2dc12fc6b73bb71670adb318058f))
* **governance-auth:** record the pinned-port decision and bring the manual current ([#205](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/205)) ([15823b5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/15823b5e8977c9f66da25fc3864d42586fe7dd0a)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **governance-auth:** retire token exchange, make --device-code the login ([#202](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/202)) ([80f87ef](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/80f87ef8d81916e537763ba91c984774c0cfab1c)), closes [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
* **integrations:** add Claude Code managed settings rollout guide ([#32](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/32)) ([26d68d4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/26d68d49a8839482be0d7ba75083b3694f18faf0))
* **integrations:** add Claude Code managed settings rollout guide ([#32](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/32)) ([#52](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/52)) ([6baa6d1](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/6baa6d108b434120d8e4a1c3d50aa7e91198afc2))
* **integrations:** add Codex telemetry rollout guide ([#33](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/33)) ([409523f](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/409523fe098010c4c119f988d58174fb533a5701))
* **integrations:** sequence diagrams for every support-matrix row ([#128](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/128)) ([de05b7c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/de05b7c7f9836a5eb0c71608d8588da116a2821b))
* **redact:** ADR-0010 bidirectional scanning + architecture docs + live test script ([cc47f2f](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cc47f2f7d27ee3ca0267bd9e601eeca706bedffe))
* **rfc:** copy source-of-truth specs into the repo, stop citing ~/Downloads ([d98bfd4](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/d98bfd4a37c3673dc41931791a2827ab469b41cb))
* **rfc:** land the Claude Code / Codex usage investigation as a source ([cf6d722](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cf6d72234ec3cec8911a2b6032b47de155a19254))
* **rfc:** RFC-0003 telemetry source taxonomy, and ADR-0013 ingest invariants ([#157](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/157)) ([a171f8c](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/a171f8c62de5e83a080df7aa092e001287a0c29c))
* **runbook:** document Content-Length root cause, fix verification, and production config ([62d5ac3](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/62d5ac3262fc74de6d70811d41a02af17d7369ba))
* **spike:** record Codex OTel admin config findings ([#34](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/34)) ([#51](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/51)) ([30a82b9](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/30a82b9c133fdf7234e5514daf1696fc1770011a))
* **spike:** record the [#7](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/7) App-token finding and spike runner ([#41](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/41)) ([cc98bda](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/cc98bdaaff85b20f7b02aaacebbef6b0c9eeddeb))
* **spike:** record the 0007 empirical run results and fix org-case match ([#46](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/46)) ([bb063f5](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/bb063f58a0ceab8ec58b54cefbbef2ad807c4184))

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
