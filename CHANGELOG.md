# Changelog

All notable changes to this project are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses [semantic versioning](https://semver.org/).

## [Unreleased]

### Changed

- Toolchains: Python 3.15, Java 25, Go 1.27, Rust 1.99 (edition 2024), Node 26 LTS; .NET stays on 10 with a pinned `global.json`.
- TypeScript port: TypeScript 7, Vitest 5 (Vite 8).
- Java port: Jackson 3 (`tools.jackson`), JUnit 6.
- C# port: xUnit v3 on Microsoft.Testing.Platform (`dotnet test` opts in via `global.json`).
- CI: `actions/checkout` v7 and current `setup-*` actions; runtime versions come from `global.json`, `go.mod` and `ts/.nvmrc`.

## [0.1.0] - 2026-08-27

First public release. per-model token/request budgets across sliding windows, scoped per tenant/user, in Python, C#, and Java.

[0.1.0]: https://github.com/parag-labs/quota-gate/releases/tag/v0.1.0
