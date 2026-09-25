"""Generate large shared Jsonnet source without committing megabytes of fixtures."""

import argparse
import json
from pathlib import Path


LIBRARIES = 8
CONSTRUCTORS = 512
RECORDS = 8192


def generate(root: Path, environments: int) -> None:
    lib = root / "lib"
    lib.mkdir(parents=True, exist_ok=True)
    (root / "jsonnetfile.json").write_text(json.dumps({
        "version": 1, "dependencies": [], "legacyImports": True,
    }))

    # Literal source makes parsing/analysis expensive; a Jsonnet comprehension
    # would instead measure the runtime work that prepared imports cannot reuse.
    inventory = {
        f"cluster-{i:04d}": {
            "region": f"region-{i % 12}",
            "provider": ["aws", "gcp", "azure"][i % 3],
            "endpoint": f"https://cluster-{i:04d}.example.com",
            "labels": {"team": f"team-{i % 64}", "tier": f"tier-{i % 4}"},
            "limits": {"cpu": 8 + i % 24, "memory": f"{16 + i % 48}Gi"},
        }
        for i in range(RECORDS)
    }
    (lib / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")

    for library in range(LIBRARIES):
        constructors = [
            f"""  config{i:04d}(name, cluster):: {{
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {{ name: name, labels: cluster.labels }},
    data: {{
      library: '{library}',
      constructor: '{i}',
      region: cluster.region,
      provider: cluster.provider,
      endpoint: cluster.endpoint,
      cpu: std.toString(cluster.limits.cpu),
      memory: cluster.limits.memory,
    }},
  }},
"""
            for i in range(CONSTRUCTORS)
        ]
        (lib / f"resources-{library}.libsonnet").write_text(
            "{\n" + "".join(constructors) + "}\n")

    imports = ",\n".join(
        f"  import 'resources-{i}.libsonnet'" for i in range(LIBRARIES))
    (lib / "main.libsonnet").write_text(f"""local inventory = import 'inventory.json';
local libraries = [
{imports},
];
{{
  resources(environment):: [
    libraries[i]['config%04d' % ((environment * {LIBRARIES} + i) % {CONSTRUCTORS})](
      'env-%04d-library-%d' % [environment, i],
      inventory['cluster-%04d' % ((environment * 127 + i) % {RECORDS})],
    )
    for i in std.range(0, {LIBRARIES - 1})
  ],
}}
""")

    for i in range(1, environments + 1):
        env = root / f"static-{i:04d}"
        env.mkdir(exist_ok=True)
        (env / "spec.json").write_text(json.dumps({
            "apiVersion": "tanka.dev/v1alpha1",
            "kind": "Environment",
            "metadata": {"name": f"library-env-{i:04d}"},
            "spec": {
                "apiServer": "https://localhost:6443",
                "namespace": f"library-env-{i:04d}",
            },
        }))
        (env / "main.jsonnet").write_text(
            f"local lib = import 'main.libsonnet';\nlib.resources({i})\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--environments", type=int, default=64)
    args = parser.parse_args()
    if args.environments < 1:
        parser.error("--environments must be positive")
    generate(args.output, args.environments)
