"""Flask server exposing the `/api/items` CRUD routes."""

from flask import Flask, jsonify, request

from backend.store import ItemStore

app = Flask(__name__)
store = ItemStore()


@app.route("/api/items", methods=["GET"])
def list_items():
    return jsonify(store.all())


@app.route("/api/items", methods=["POST"])
def add_item():
    data = request.get_json()
    store.add(data["name"])
    return jsonify({"status": "ok"}), 201


@app.route("/api/items/<int:item_id>", methods=["DELETE"])
def remove_item(item_id: int):
    store.remove(item_id)
    return "", 204
