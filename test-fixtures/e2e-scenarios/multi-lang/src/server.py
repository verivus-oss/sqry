def handle_request(req):
    return transform(req.body)


def transform(data):
    return data.upper()


class RequestHandler:
    def __init__(self):
        self.name = "handler"

    def process(self, req):
        return handle_request(req)
