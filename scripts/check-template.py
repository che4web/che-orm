#!/usr/bin/env python3
"""Generate and verify standalone applications against local or published che-orm."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def run(command, *, cwd, env):
    print("$ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["sqlite", "postgres"])
    parser.add_argument(
        "--published", action="store_true",
        help="use crates.io instead of patching core and macros to this checkout",
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", str(root / "target" / "template-check"))
    # Schema export must work without opening this intentionally invalid URL.
    env["CHE_ORM_DATABASE_URL"] = "invalid://template-schema-check"
    patches = [] if args.published else [
        "--config", f'patch.crates-io.che-orm.path="{root.as_posix()}"',
        "--config", f'patch.crates-io.che-orm-macros.path="{(root / "che-orm-macros").as_posix()}"',
    ]

    def cargo(*args):
        return ["cargo", args[0], *patches, *args[1:]]

    for backend in [args.backend] if args.backend else ["sqlite", "postgres"]:
        with tempfile.TemporaryDirectory(prefix="che-orm-template-") as directory:
            name = f"orm-template-{backend}"
            # Also exercise generation inside an existing Cargo workspace.
            parent_manifest = Path(directory) / "Cargo.toml"
            parent_contents = '[workspace]\nmembers = []\n'
            parent_manifest.write_text(parent_contents)
            run(
                [
                    "cargo", "generate", "--path", str(root / "templates" / "che-orm"),
                    "--name", name, "--destination", directory,
                    "--define", f"backend={backend}", "--silent", "--vcs", "none",
                    "--no-workspace",
                ],
                cwd=root,
                env=env,
            )
            assert parent_manifest.read_text() == parent_contents
            project = Path(directory) / name
            for command in [
                ["cargo", "fmt", "--check"],
                cargo("test"),
                cargo("clippy", "--all-targets", "--", "-D", "warnings"),
                cargo("run", "--bin", "manage", "--", "schema"),
            ]:
                run(command, cwd=project, env=env)

            if backend == "sqlite":
                runtime_env = dict(env, CHE_ORM_DATABASE_URL=str(project / "app.db"))
                for command in [
                    cargo("run", "--bin", "manage", "--", "migrate"),
                    cargo("run", "--bin", "manage", "--", "migrate"),
                    cargo("run", "--bin", "manage", "--", "migrate", "status"),
                    cargo("run"),
                ]:
                    run(command, cwd=project, env=runtime_env)
            else:
                run(
                    cargo("test", "--no-default-features", "--features", "sqlite"),
                    cwd=project,
                    env=env,
                )


if __name__ == "__main__":
    main()
