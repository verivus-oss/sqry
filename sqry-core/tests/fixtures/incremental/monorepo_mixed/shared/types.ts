// Types shared between backend (TypeScript side) and frontend.

export interface Item {
    id: number;
    name: string;
}

export interface ApiResponse<T> {
    status: "ok" | "error";
    data?: T;
    message?: string;
}
