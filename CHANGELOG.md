# 0.37.6

## Provider keys and command completion in OpenTUI

The OpenTUI frontend now lists saved provider credential names with `/keys`,
accepts API keys through a concealed `/key set <provider>` or `/login <provider>`
prompt, and removes credentials with `/key remove <provider>`. It never renders
or reads back saved key values. Tab completes native and backend slash commands
plus available session, model, Workflow, provider, interaction, and API route
arguments.

The `hya.v1` Auth service now includes `ListProviderAuth` over
`GET /v1/auth`, returning only sorted provider IDs with saved credentials.
The server writes new and replacement API-key files with owner-only
permissions on Unix. Restart the backend after a credential change to apply it
to configured provider routes.
