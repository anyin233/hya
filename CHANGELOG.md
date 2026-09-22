# 0.37.6

## Agentless Plugin packages

Public `Plugin` packages distribute resource capabilities without an Agent,
Workflow, or channel. Strict YAML preparation, canonical prepared-v2 encoding,
package writing, mixed catalogs, and the existing bundle CLI lifecycle support
this slim hyabundle payload. Agent and Workflow fields are rejected.

Static Skills publish through the existing immutable runtime registry. Process
and MCP declarations remain validated package metadata; automatic agentless
process/MCP startup is follow-up work.
