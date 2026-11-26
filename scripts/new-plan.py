"""Create a local plan from the shared Markdown template."""

import re
from pathlib import Path


def main():
    root = Path(__file__).resolve().parents[1]
    template = (root / "docs/templates/plan.md").read_text(encoding="utf-8")
    plans = root / ".plan"
    plans.mkdir(exist_ok=True)
    numbers = (int(p.name[1:]) for p in plans.iterdir() if re.fullmatch(r"p[0-9]+", p.name))
    number = max(numbers, default=0) + 1
    while True:
        directory = plans / f"p{number}"
        try:
            directory.mkdir()
            break
        except FileExistsError:
            number += 1
    path = directory / "task_plan.md"
    path.write_text(template.replace("{plan_id}", directory.name), encoding="utf-8")
    print(path.relative_to(root))


if __name__ == "__main__":
    main()
