import uuid
from flask import Flask, Response

app = Flask(__name__)

@app.route("/")
def generate_uuid():
    return Response(f"{uuid.uuid4()}", content_type="text/plain")

@app.route("/<int:count>")
def generate_uuids(count: int):
    body = "\n".join(str(uuid.uuid4()) for _ in range(max(1, min(count, 1024))))
    return Response(body, content_type="text/plain")

if __name__ == "__main__":
    app.run()
