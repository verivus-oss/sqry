// Hand-written ServiceNow server-side JavaScript sample for body-shape coverage.
// Models a typical business-rule script include: branch, loop, switch, try/catch/
// throw/finally, return, break/continue, calls (GlideRecord + new), assignment,
// and a closure callback.
function processIncidents(priority, region) {
    var total = 0;

    if (priority > 2) {
        total = priority;
    } else {
        total = 0;
    }

    var gr = new GlideRecord('incident');
    gr.addQuery('priority', priority);
    gr.query();
    while (gr.next()) {
        if (gr.getValue('state') === '7') {
            continue;
        }
        if (total > 100) {
            break;
        }
        total += parseInt(gr.getValue('impact'), 10);
    }

    for (var i = 0; i < region.length; i++) {
        total += region[i];
    }

    switch (total) {
        case 1:
            total = 1;
            break;
        default:
            total = 2;
    }

    try {
        total = compute(total);
    } catch (err) {
        throw new Error('compute failed');
    } finally {
        cleanup();
    }

    var callback = function (value) {
        return value * 2;
    };

    return callback(total);
}
