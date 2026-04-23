// Webhook receiver. Pass 5 should match `client.sendWebhook` against this
// route handler.

import express from "express";
import type { WebhookPayload } from "./types";

const router = express.Router();

router.post("/api/webhooks/github", (req, res) => {
    const payload: WebhookPayload = req.body;
    // Intentionally ignore the payload — we only care about the route.
    res.status(200).json({ received: payload.event });
});

export default router;
