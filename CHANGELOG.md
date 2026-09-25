# 0.37.7

## OpenTUI startup with an older backend

The OpenTUI frontend now remains usable when a backend predating `GET /v1/auth`
returns 404 during startup. The `/keys` view states that key listing is
unavailable until the backend is updated and restarted. Empty or non-JSON HTTP
error bodies now show the failing method, path, and status instead of a null
object exception.
