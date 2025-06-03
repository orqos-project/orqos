import os, osproc, json, streams

let dockerSock = getEnv("DOCKER_HOST_SOCKET_PATH", "/var/run/docker.sock")

let curlCmd = [
  "curl",
  "--no-buffer",  # disables curl's output buffering
  "--silent",     # suppresses progress meter and error messages
  "--unix-socket", dockerSock,  # path to Docker's Unix socket
  "http://localhost/events"
]

# Start curl and capture its stdout
let p = startProcess(
  curlCmd[0], args = curlCmd[1..^1],
  options = {poStdErrToStdOut, poUsePath}
)

var line: string
let s = p.outputStream

while streams.readLine(s, line):
  try:
    echo line

    let evt = json.parseJson(line)
    # Pretty print event (or do your own processing)
    echo evt.pretty()
  except Exception as e:
    echo "Failed to parse event: ", e.msg, "\nRaw: ", line

# Clean up
p.close()
