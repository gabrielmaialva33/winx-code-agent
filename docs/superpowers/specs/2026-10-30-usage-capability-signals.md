# Usage metadata and backend discovery

## Intent
Fix CodeMap payload/source metrics and false empty temporary measurements reproduced in the 2026-10-30 audit. Help plugin clients distinguish a disabled Herdr adapter from an installed CLI.

## Approach
Correct the collector, not tool outputs. Canonical typed snake_case root fields take priority; legacy layouts stay supported. Emit optional temporary measurements and temporary_usage_schema=1; absent/malformed values must not become observed zeros. Historical zeros remain unverifiable.

Add agent_backends.herdr in successful Initialize responses, including status. Report only whether WINX_HERDR_CONFIG is present: absent means disabled; present means unknown until validated. Do not read config files, inspect targets, launch Herdr, expose paths or grant permissions during discovery. Disabled failures require operator configuration and confirm input was not sent.

## Alternatives and boundaries
Keeping default zeros preserves misinformation. Probing Herdr during Initialize adds IO and authority complexity, so use a conservative presence-only hint. No changes to polling, output content, cleanup, dependencies, permissions or deployment. No commit/push without a separate request.

## Verification
Tests first: full CodeMap envelopes, legacy layouts/precedence, missing/malformed versus measured-zero temporary values, overflow records, discovery/failure hints and privacy. Exercise HTTP usage tests and run just check with bounded parallelism. Rust 2021; MSRV 1.90.
