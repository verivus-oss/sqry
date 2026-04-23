const { computeValue } = require('./helper');

function processData() {
    return computeValue(42);
}

module.exports = { processData };
