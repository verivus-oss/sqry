# Python Flask server with route handlers
from flask import Flask, jsonify, request

app = Flask(__name__)

@app.route("/api/users")
def list_users():
    return jsonify({"users": []})

@app.post("/api/items")
def create_item():
    data = request.get_json()
    return jsonify({"created": True})
