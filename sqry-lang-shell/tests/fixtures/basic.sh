#!/bin/bash

foo() {
  echo "foo"
}

function bar {
  echo "bar"
}

export -f bar
export DATA_PATH="/tmp/data"
