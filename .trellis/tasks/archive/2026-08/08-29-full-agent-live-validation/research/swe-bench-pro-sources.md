# SWE-Bench Pro pinned source record

## Approved pins

- Evaluator repository: `scaleapi/SWE-bench_Pro-os`
- Evaluator revision: `ca10a60a5fcae51e6948ffe1485d4153d421e6c5`
- Dataset repository: `ScaleAI/SWE-bench_Pro`
- Dataset revision: `7ab5114912baf22bb098818e604c02fe7ad2c11f`

These pins come from the approved plan in `approved-plan.md`. Do not advance either revision during this task.

## Primary sources checked

1. Evaluator README at the pinned revision:
   `https://raw.githubusercontent.com/scaleapi/SWE-bench_Pro-os/ca10a60a5fcae51e6948ffe1485d4153d421e6c5/README.md`
   - Defines SWE-Bench Pro as a patch-generation benchmark.
   - Identifies the public Hugging Face dataset and the `jefzda/sweap-images` Docker repository.
   - Documents local Docker evaluation with `swe_bench_pro_eval.py`.
2. Evaluator license at the pinned revision:
   `https://raw.githubusercontent.com/scaleapi/SWE-bench_Pro-os/ca10a60a5fcae51e6948ffe1485d4153d421e6c5/LICENSE`
   - MIT License, copyright Scale AI, Inc.
3. Evaluator script at the pinned revision:
   `https://raw.githubusercontent.com/scaleapi/SWE-bench_Pro-os/ca10a60a5fcae51e6948ffe1485d4153d421e6c5/swe_bench_pro_eval.py`
   - Lines 470-553 build `eval_results`, set failures and exceptions to `false`, require all `fail_to_pass` and `pass_to_pass` tests to be present in the passed set, and write `eval_results.json`.
   - The task must read those Booleans. Process exit status alone is not a score.
4. Dataset README at the pinned revision:
   `https://huggingface.co/datasets/ScaleAI/SWE-bench_Pro/resolve/7ab5114912baf22bb098818e604c02fe7ad2c11f/README.md`
   - Declares one `test` split with 731 examples.
   - Declares the fields used by the approved sampler and prompt builder, including `repo`, `instance_id`, `base_commit`, `problem_statement`, `requirements`, `interface`, `repo_language`, and `dockerhub_tag`.
   - The metadata does not declare a dataset license. Do not redistribute rows, repository snapshots, or images.

## Execution contract

- Use `uv` for Python environment and dependency work.
- Freeze selected instance IDs, row hashes, prompt hashes, base commits, Docker tags, digests, and image platforms before the first provider request.
- Keep gold patches, test patches, test names, scripts, and evaluator-only fields outside Hya worktrees and Docker mounts.
- Run backend and Herdr/TUI predictions in separate evaluator invocations because evaluator results are keyed only by `instance_id`.
- Preserve every Boolean, including `false`. This is diagnostic Pass@1, not an official leaderboard reproduction.
