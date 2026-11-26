#!/usr/bin/env python3
"""Bound the benchmark's dependency setup; never retry a measurement."""
import os
import subprocess
import sys


def prepare(*, run=subprocess.run, cargo=None):
    # Fetch without --target or package filters: Cargo includes the workspace's
    # dev/optional dependencies and target-specific graph, including benchmarks.
    command = [*(cargo or ['cargo']), 'fetch', '--locked']
    env = dict(os.environ, CARGO_NET_RETRY='0', CARGO_HTTP_TIMEOUT='30',
               CARGO_NET_OFFLINE='false')
    try:
        run(command, env=env, timeout=180, check=True)
    except subprocess.TimeoutExpired:
        print('::error::Dependency infrastructure setup timed out; no benchmark results.',
              file=sys.stderr)
        return 1
    except (subprocess.CalledProcessError, OSError) as error:
        print(f'::error::Dependency infrastructure setup failed ({error}); '
              'no benchmark results. See Cargo diagnostics above.', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(prepare())
