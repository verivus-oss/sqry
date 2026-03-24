class ErrorHandler {
    func validate() throws {
        try process()
    }

    private func process() throws {
        // Processing
    }

    public func publicMethod() throws {
        try validate()
    }

    internal func internalMethod() {
        // No throws
    }
}
