"""Pre-migration guard: detect and heal superseded development migration stamps.

Run automatically by ``make api-migrate`` before ``alembic upgrade head``.

Background
----------
Five individual development migrations were squashed into the master migration
``27a5afe0a297_add_workspace_slug.py``.  Databases that were stamped with any
of those superseded revisions will cause Alembic to abort with a "Can't locate
revision" error because the original migration files have been deleted.

This script detects that situation and auto-stamps the database to the squashed
head so that ``alembic upgrade head`` can proceed normally.
"""

import subprocess
import sys

# Squashed master revision — all superseded hashes map to this.
SQUASHED_HEAD = "27a5afe0a297"

# Hashes that were squashed (migration files no longer exist).
SUPERSEDED_HASHES = {
    "f61f165b24ed",
    "7c56e7c956bf",
    "9dda0b1277c4",
    "f61f165b24ee",
    "3c2fc93e2b17",
}


def main() -> None:
    # Run ``alembic current`` — may print an error if the DB is on a revision
    # whose file has been deleted (exactly the case we need to heal).
    result = subprocess.run(
        ["alembic", "current"],
        capture_output=True,
        text=True,
    )
    output = (result.stdout + result.stderr).lower()

    needs_stamp = any(h in output for h in SUPERSEDED_HASHES)

    if not needs_stamp:
        if result.returncode != 0:
            print(
                "ERROR: 'alembic current' failed. Misconfigured DB environment?\n"
                f"{result.stderr}",
                file=sys.stderr,
            )
            sys.exit(result.returncode)
        # DB is on a known-good revision (or brand-new); nothing to do.
        return

    print(
        "\n⚠️  WARNING: Database is stamped with a superseded development migration.",
        file=sys.stderr,
    )
    print(
        f"   These migrations were squashed into '{SQUASHED_HEAD}'.",
        file=sys.stderr,
    )
    print(
        f"   Stamping the database to the squashed head automatically...\n",
        file=sys.stderr,
    )

    stamp = subprocess.run(
        ["alembic", "stamp", SQUASHED_HEAD],
        capture_output=True,
        text=True,
    )
    if stamp.returncode != 0:
        print(
            f"ERROR: Failed to stamp database to '{SQUASHED_HEAD}':\n{stamp.stderr}",
            file=sys.stderr,
        )
        sys.exit(1)

    print(
        f"✅  Successfully stamped database to '{SQUASHED_HEAD}'.",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
