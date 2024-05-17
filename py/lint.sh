#!/bin/bash
set -ex

black py/maelstrom_client/client.py
black py/maelstrom_client/__init__.py
black py/maelstrom_pytest.py

mypy --ignore-missing-imports py/maelstrom_client/client.py
mypy --ignore-missing-imports py/maelstrom_client/__init__.py
mypy --ignore-missing-imports py/maelstrom_pytest.py