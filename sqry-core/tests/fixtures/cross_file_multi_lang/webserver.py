from flask import Flask, jsonify, request

app = Flask(__name__)

@app.route('/api/users', methods=['GET'])
def get_users():
    return jsonify([{"id": 1, "name": "Alice"}])

@app.route('/api/users', methods=['POST'])
def create_user():
    data = request.get_json()
    return jsonify({"id": 2, "name": data["name"]}), 201
