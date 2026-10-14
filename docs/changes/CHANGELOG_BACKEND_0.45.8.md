# 0.45.8

## Storage

- Serialize bundle-registry migration discovery and application so concurrent first opens cannot apply the same migration twice. Setup retains its bounded lock retries and registry mutations remain fail-fast.
