---
title: "The target path"
description: "Where DRE writes compiled SQL, outputs, snapshots and the manifest."
sidebar:
  order: 12
---

# The target path

The target path is the folder DRE writes its generated files to. It has nothing to do with a
run's target (`--target`, the environment every profile uses). It's `target/` in the project unless set,
highest first, by:

1. `--target-path <path>` on `compile`, `validate`, `run`, `clean` and `ls`;
2. `DRE_TARGET_PATH`;
3. `target_path:` in `dre_project.yml`.

A relative path is relative to the project root, whichever of the three sets it; `~` is your home
directory. Compiled SQL, run outputs, schema snapshots, `run_results.json` and the manifest move
together. The path must be local or mounted: object storage URLs (`s3://`, `gs://`, `abfss://`)
are refused, but a filesystem mount that supports renames works, such as a Databricks Unity
Catalog Volume (`/Volumes/...` on Databricks compute), an NFS/EFS share or a gcsfuse mount. DRE
refuses a target path that is the project root, contains the project, or sits inside `reports/`,
`macros/`, `lookups/` or `dre_deps/`. A path elsewhere inside the project is skipped when DRE
reads the project; add it to `.gitignore` (a new project's `.gitignore` covers `target/` only).

Schema-drift detection compares each run with the snapshot the last successful run left in the
target path, so on an ephemeral runner (a job cluster, a CI runner) point it at a folder that
outlives the run:

```bash
dre run --schedule close_monthly --target-path /mnt/shared/dre/target
```

In a Databricks job, a Volume keeps the snapshots and the manifest between runs:

```bash
export DRE_TARGET_PATH=/Volumes/main/reporting/dre/target
dre run --schedule close_monthly
```

Give each job that runs at the same time its own target path; two runs sharing one overwrite
each other's files and snapshots. `dre clean` deletes the target folder only if DRE created it
(it leaves a `.dre_target` file there) or it's the project's own `target/`, so a mistyped
`--target-path` can't delete anything else.
