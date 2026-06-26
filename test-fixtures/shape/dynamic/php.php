<?php
// Hand-written PHP control-flow sample for shape descriptor coverage.

function classify(int $value, string $label = "n/a", ...$rest): string
{
    $result = 0;
    if ($value > 100) {
        $result = 1;
    } elseif ($value > 10) {
        $result = 2;
    } else {
        $result = 3;
    }

    switch ($value) {
        case 0:
            return "zero";
        case 1:
            return "one";
        default:
            $result = 9;
    }

    while ($result > 0) {
        $result--;
        if ($result == 2) {
            continue;
        }
        if ($result < 0) {
            break;
        }
    }

    for ($i = 0; $i < 3; $i++) {
        helper($i);
    }

    foreach ($rest as $entry) {
        helper($entry);
    }

    $double = fn($x) => $x * 2;
    $closure = function ($y) use ($result) {
        return $y + $result;
    };

    try {
        if ($value < 0) {
            throw new InvalidArgumentException("bad");
        }
        helper($value);
    } catch (InvalidArgumentException $e) {
        $result = -1;
    } finally {
        cleanup();
    }

    return $label . $result;
}

function helper($x)
{
    return $x + 1;
}

function cleanup(): void
{
}
