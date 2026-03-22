protocol DataProcessor {
    func process()
}

extension DataProcessor {
    func validate() {
        // Extension method
        process()
    }

    func transform() {
        validate()
    }
}

enum Status {
    case active
    case inactive

    func description() -> String {
        return "status"
    }
}

actor Cache {
    func store() {
        // Actor method
    }

    func retrieve() {
        store()
    }
}
