#!/usr/bin/env bash
# Hand-written shell control-flow sample for shape descriptor coverage.

classify() {
  local value="$1"
  local label="${2:-n/a}"
  local result=0

  if [ "$value" -gt 100 ]; then
    result=1
  elif [ "$value" -gt 10 ]; then
    result=2
  else
    result=3
  fi

  case "$value" in
    0)
      echo "zero"
      ;;
    1 | 2 | 3)
      echo "small"
      ;;
    *)
      echo "large"
      ;;
  esac

  while [ "$result" -gt 0 ]; do
    result=$((result - 1))
    if [ "$result" -eq 2 ]; then
      continue
    fi
    if [ "$result" -lt 0 ]; then
      break
    fi
  done

  for i in 1 2 3; do
    helper "$i"
  done

  until [ "$result" -ge 5 ]; do
    result=$((result + 1))
  done

  helper "$value"
  return "$result"
}

helper() {
  echo "$(($1 + 1))"
}
