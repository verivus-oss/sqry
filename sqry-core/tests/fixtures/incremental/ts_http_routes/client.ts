// Typed HTTP client that issues requests against the routes declared in
// `server.ts` and `webhook.ts`. Exercises the Pass 5 HTTP-request linker.

import type { User, WebhookPayload } from "./types";

export async function fetchUsers(): Promise<User[]> {
    const response = await fetch("/api/users");
    return response.json();
}

export async function createUser(name: string): Promise<User> {
    const response = await fetch("/api/users", {
        method: "POST",
        body: JSON.stringify({ name }),
    });
    return response.json();
}

export async function deleteUser(id: number): Promise<void> {
    await fetch(`/api/users/${id}`, { method: "DELETE" });
}

export async function sendWebhook(payload: WebhookPayload): Promise<void> {
    await fetch("/api/webhooks/github", {
        method: "POST",
        body: JSON.stringify(payload),
    });
}
