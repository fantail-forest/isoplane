#!/usr/bin/env python3
"""Prepare shared fixtures before starting parallel test binaries."""

from pathlib import Path

import tasks


if __name__ == "__main__":
    tasks.run(Path(__file__).resolve().parents[1], "fixtures")
