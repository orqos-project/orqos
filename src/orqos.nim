import os, osproc, json, streams, posix

let dockerSock = getEnv("DOCKER_HOST_SOCKET_PATH", "/var/run/docker.sock")

proc socketExists(path: string): bool =
  var st: Stat
  if stat(path, st) != 0:
    return false
  return S_ISSOCK(st.st_mode)

if not socketExists(dockerSock):
  echo "Error: Docker socket not found at: ", dockerSock
  quit(1)

let curlCmd = [
  "curl",
  "--no-buffer",  # disables curl's output buffering
  "--silent",     # suppresses progress meter and error messages
  "--unix-socket", dockerSock,  # path to Docker's Unix socket
  "http://localhost/events"
]

# Start curl and capture its stdout
let p = try:
  startProcess(
    curlCmd[0], args = curlCmd[1..^1],
    options = {poStdErrToStdOut, poUsePath}
  )
except OSError as e:
  echo "Failed to start curl process: ", e.msg
  quit(1)

var line: string
let s = p.outputStream

try:
  while streams.readLine(s, line):
    try:
      echo line

      let evt = json.parseJson(line)
      # Pretty print event (or do your own processing)
      echo evt.pretty()
    except JsonParsingError as e:
      echo "Failed to parse event: ", e.msg, "\nRaw: ", line
    except Exception as e:
      echo "Unexpected error processing event: ", e.msg
      break
finally:
  # Ensure process cleanup happens even if exceptions occur
  if p != nil:
    p.close()
