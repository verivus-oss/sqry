"""Backend API server - demonstrates HTTP endpoints and database calls"""
from flask import Flask, request, jsonify
import native_lib  # FFI to C++ library

app = Flask(__name__)

def authenticate_request():
    """Authenticate incoming request"""
    auth_header = request.headers.get('Authorization')
    if not auth_header:
        raise ValueError('Missing authorization')
    # Calls native C++ library for auth validation
    return native_lib.validate_token(auth_header)

@app.route('/api/users', methods=['GET'])
def get_users():
    """
    GET /api/users - Fetch all users

    This endpoint demonstrates:
    - HTTP request handling
    - Database query
    - Authentication check
    """
    # Authenticate
    user_id = authenticate_request()

    # Query database
    users = query_database('SELECT * FROM users')

    return jsonify(users)

@app.route('/api/users', methods=['POST'])
def create_user():
    """
    POST /api/users - Create a new user

    This endpoint demonstrates:
    - HTTP request handling
    - Input validation
    - Database write operation
    - FFI call to native library
    """
    # Authenticate
    user_id = authenticate_request()

    # Get request data
    data = request.json

    # Validate
    validated = validate_user_data(data)

    # Hash password using native library
    hashed_password = native_lib.hash_password(validated['password'])
    validated['password'] = hashed_password

    # Save to database
    user_id = save_to_database('users', validated)

    return jsonify({'id': user_id, **validated}), 201

def validate_user_data(data):
    """Validate user input data"""
    required = ['username', 'email', 'password']
    for field in required:
        if field not in data:
            raise ValueError(f'Missing required field: {field}')
    return data

def query_database(query):
    """Execute database query"""
    # Placeholder for actual database call
    # In real code, this would use SQLAlchemy or similar
    print(f'Executing query: {query}')
    return []

def save_to_database(table, data):
    """Save data to database"""
    # Placeholder for actual database write
    print(f'Saving to {table}: {data}')
    return 1

if __name__ == '__main__':
    app.run(debug=True, port=5000)
