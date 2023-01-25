import os
import redis
import sys
import typing


def listdir(root: str, acc: str = "") -> typing.Iterator[str]:
    for entry in os.scandir(root):
        if entry.is_dir():
            for entry in listdir(f"{root}/{entry.name}", f"{acc}/{entry.name}" if acc else entry.name):
                yield entry
        else:
            yield f"{acc}/{entry.name}" if acc else entry.name


if len(sys.argv) == 1:
    print(f"usage: {sys.argv[0]} <sync>")
    exit(1)

if sys.argv[1] == "sync" and len(sys.argv) == 2:
    print(f"usage: {sys.argv[0]} sync <directory>")
    exit(1)

redis_url = os.environ.get("REDIS_URL")
if redis_url is None:
    print("missing environment variable REDIS_URL")
    exit(1)

redis = redis.Redis.from_url(redis_url)

mimetypes = {
    "html": b"text/html",
    "css": b"text/css",
    "js": b"application/javascript",
    "svg": b"image/svg+xml",
    "ico": b"image/vnd.microsoft.icon",
    "woff2": b"font/woff2",
    "png": b"image/png",
}

if sys.argv[1] == "sync":
    directory = sys.argv[2]

    local = []
    for path in listdir(directory):
        local.append((path[:-11] if path.endswith("index.html")
                     else path, f"{directory}/{path}"))

    remote = list(map(lambda x: x[6:], redis.scan_iter("asset:*")))

    for item in local:
        try:
            remote.remove(item[0].encode("utf8"))
        except ValueError:
            pass

        with open(item[1], "rb") as f:
            buffer = mimetypes.get(item[1].rsplit(
                ".")[-1], b'text/plain') + b";"
            while (data := f.read()) != b"":
                buffer += data
            redis.set(f"asset:{item[0]}", buffer)
        redis.publish("invalidations", item[0])
        print(f"uploaded {item[1]} to asset:{item[0]}")

    for item in remote:
        key = f"asset:{item.decode('utf8')}"
        redis.expire(key, 60 * 60 * 24)
        print(f"expired {key}")
