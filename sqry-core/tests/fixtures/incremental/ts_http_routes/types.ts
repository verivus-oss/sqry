// Shared types imported by both client and server.

export interface User {
    id: number;
    name: string;
}

export interface WebhookPayload {
    event: string;
    deliveryId: string;
    repository?: string;
}
