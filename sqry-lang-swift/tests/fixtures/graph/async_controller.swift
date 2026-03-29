import Foundation

class UserController {
    func create() async {
        await sendWelcomeEmail()
        audit()
    }

    func sendWelcomeEmail() async {
        Mailer.deliver()
    }

    static func deliver() {
        // Static method
    }

    func audit() {
        self.log()
    }

    private func log() {
        // Private logging
    }
}
