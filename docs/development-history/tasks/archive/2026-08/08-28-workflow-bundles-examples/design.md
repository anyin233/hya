# Design

The parent design section **Bundle and Plugin Modules** is authoritative.

## Payload Seam

Dispatch source manifests by `kind` into separate strict structs. Preserve `AgentBundle` and add `WorkflowBundle`; prepare both into `PreparedInstallableBundle::{Agent, Workflow}` format v2. The registry remains one row per bundle identity.

A WorkflowBundle stores one source plus every direct Stage/verifier Agent and reachable non-built-in helper Agent. Preparation calls `hya_workflow::compile`, then verifies identity, closure, files, digests, paths, resources, extensions, collisions, and canonical order.

## Catalog Seam

`BundleCatalog` uses generic bundle slots plus Agent indexes and Workflow indexes. Both payload kinds publish Agents with `AgentOrigin::Bundle`. Workflow sources enter one immutable `WorkflowCatalog`; first-party, installed, user, and project precedence follows the parent design.

## Plugin Seam

`PluginContributionSet` is the one interface for tools, Skills, hooks, and workspace adapters. External hosts and the prepared static adapter are two real adapters. Signed bundle Skills remain host-authoritative: any process declaration must match selected prepared bytes and digest.

## Distribution

Package the Compat adapter under adjacent `lib/hya`, with environment override then installed then workspace resolution. Deterministic package generation owns release `.hyabundle` bytes. The simple bundle is cataloged but never selected; the full Argus bundle is shipped as an ordinary installable example.
