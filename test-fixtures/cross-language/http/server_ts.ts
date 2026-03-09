import express from "express";

const app = express();

app.all("/health", (req, res) => {
    res.json({ status: "ok" });
});

app.get("/api/users", (req, res) => {
    res.json({ users: [] });
});

app.listen(3000);
