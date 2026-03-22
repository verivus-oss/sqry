// Frontend API client - demonstrates HTTP calls to backend
import axios from 'axios';

/**
 * Fetch all users from the backend API
 * @returns {Promise<Array>} List of users
 */
export async function fetchUsers() {
    const response = await axios.get('/api/users');
    return response.data;
}

/**
 * Create a new user
 * @param {Object} userData - User data to create
 * @returns {Promise<Object>} Created user
 */
export async function createUser(userData) {
    const response = await axios.post('/api/users', userData);
    return response.data;
}

/**
 * Compress data using native library (FFI call)
 * @param {string} data - Data to compress
 * @returns {Promise<Buffer>} Compressed data
 */
export async function compressData(data) {
    // This calls the native C++ compression library via FFI
    const native = require('./native-bindings');
    return native.compress(data);
}

/**
 * Main application entry point
 */
export async function main() {
    console.log('Fetching users...');
    const users = await fetchUsers();
    console.log(`Found ${users.length} users`);

    console.log('Creating new user...');
    const newUser = await createUser({
        username: 'testuser',
        email: 'test@example.com'
    });
    console.log('User created:', newUser);

    console.log('Compressing data...');
    const compressed = await compressData('Hello, World!');
    console.log('Compressed:', compressed.length, 'bytes');
}
