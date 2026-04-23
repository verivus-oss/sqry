// Helper utilities imported by the client.

import type { User } from "./types";

export function formatUser(u: User): string {
    return `${u.id}:${u.name}`;
}

export function sortUsers(users: User[]): User[] {
    return [...users].sort((a, b) => a.id - b.id);
}
