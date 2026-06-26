// Hand-written control-flow sample for the Swift body-shape descriptor coverage
// test. Exercises branch, loop, switch, do/catch, throw, return, control
// transfer, call, and assignment so the canonical CfBucket histogram is
// non-empty.

enum ClassifyError: Error {
    case negative
}

func classify(_ n: Int, label: String) -> Int {
    var total = 0
    if n > 0 {
        total = compute(n)
    } else {
        total = 0
    }

    for i in 0..<n {
        if i == 3 {
            continue
        }
        total += i
    }

    while total < 100 {
        total += 1
        if total == 50 {
            break
        }
    }

    switch n {
    case 0:
        emit(total)
    default:
        emit(0)
    }

    do {
        try mightFail(n)
    } catch {
        throw ClassifyError.negative
    }

    return total
}

func compute(_ value: Int) -> Int {
    return value * 2
}

func emit(_ value: Int) {
    _ = value
}

func mightFail(_ value: Int) throws {
    if value < 0 {
        throw ClassifyError.negative
    }
}
