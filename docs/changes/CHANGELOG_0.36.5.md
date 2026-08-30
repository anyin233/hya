# 0.36.5

## Model fallback

- Cross-model fallback now resolves each candidate's `#reasoning` variant before
  dispatch, while preserving the original request effort when a candidate has
  no recognized reasoning variant.
