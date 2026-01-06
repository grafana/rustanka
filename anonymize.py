#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""
Anonymize a file by randomizing alphanumeric characters while preserving structure.

Usage:
    ./anonymize.py <file>                    # Anonymize entire file
    ./anonymize.py <file> --lines 10-50      # Anonymize only lines 10 through 50
"""

import argparse
import random
import string
import sys
from pathlib import Path


def anonymize_char(c: str) -> str:
    """Replace alphanumeric char with random one of same type, preserve others."""
    if c.isdigit():
        return random.choice(string.digits)
    elif c.isalpha():
        if c.isupper():
            return random.choice(string.ascii_uppercase)
        else:
            return random.choice(string.ascii_lowercase)
    return c


def anonymize_text(text: str) -> str:
    """Anonymize all alphanumeric characters in text, preserving escape sequences."""
    result = []
    i = 0
    while i < len(text):
        c = text[i]
        if c == '\\' and i + 1 < len(text):
            # Preserve backslash and the following character
            result.append(c)
            result.append(text[i + 1])
            i += 2
        else:
            result.append(anonymize_char(c))
            i += 1
    return ''.join(result)


def parse_line_range(range_str: str) -> tuple[int, int]:
    """Parse a line range like '10-50' into (start, end) 1-indexed inclusive."""
    parts = range_str.split('-')
    if len(parts) != 2:
        raise ValueError(f"Invalid line range: {range_str}. Expected format: START-END")
    return int(parts[0]), int(parts[1])


def main():
    parser = argparse.ArgumentParser(
        description="Anonymize a file by randomizing alphanumeric characters."
    )
    parser.add_argument("file", type=Path, help="File to anonymize")
    parser.add_argument(
        "--lines", "-l",
        type=str,
        help="Line range to anonymize (1-indexed, inclusive), e.g. '10-50'"
    )
    args = parser.parse_args()

    if not args.file.exists():
        print(f"Error: {args.file} not found", file=sys.stderr)
        sys.exit(1)

    content = args.file.read_text()

    if args.lines:
        start, end = parse_line_range(args.lines)
        lines = content.splitlines(keepends=True)
        
        # Convert to 0-indexed
        start_idx = start - 1
        end_idx = end  # end is inclusive, so we go up to end_idx
        
        if start_idx < 0 or end_idx > len(lines):
            print(f"Error: Line range {start}-{end} out of bounds (file has {len(lines)} lines)", file=sys.stderr)
            sys.exit(1)
        
        # Anonymize only the specified range
        for i in range(start_idx, end_idx):
            lines[i] = anonymize_text(lines[i])
        
        output = ''.join(lines)
    else:
        output = anonymize_text(content)

    args.file.write_text(output)
    print(f"Anonymized: {args.file}")


if __name__ == "__main__":
    main()
