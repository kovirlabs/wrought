+++
title = "Configuration"
description = "Every setting, what it does and its default."
weight = 2
+++

Settings live in one file at the root of the project.

## General

| Setting | Default | What it does |
| --- | --- | --- |
| `name` | the folder's name | Shown in titles and the header |
| `port` | `4400` | Where the development server listens |
| `output` | `public` | Where builds are written |

## Environment

Secrets never go in the file: set them in the environment instead.

```sh
export TOOL_TOKEN="..."
```

## Precedence

Command-line flags win over the environment, which wins over the file.
