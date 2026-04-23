// Express-style route handlers for user CRUD. Pass 5 should detect the
// route definitions and wire the client's fetch calls to these handlers.

import express from "express";
import type { User } from "./types";

const app = express();

app.get("/api/users", (_req, res) => {
    const users: User[] = [{ id: 1, name: "Alice" }];
    res.json(users);
});

app.post("/api/users", (req, res) => {
    const name: string = req.body.name;
    res.status(201).json({ id: 2, name });
});

app.delete("/api/users/:id", (req, res) => {
    const id = Number(req.params.id);
    res.status(204).send(`deleted ${id}`);
});

export default app;
