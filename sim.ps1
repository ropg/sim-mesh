# sim.ps1: sim-mesh's front on Windows without a bash, with Docker or Podman.
# It does what `sim` with no arguments does: builds the image once per
# Dockerfile, then runs `sim front` inside it with port 8800 published. Every
# other verb runs inside the front once it is up:
#   docker exec -it sim-mesh-front ./sim <verb>
# Start it from the directory the clone is in:
#   powershell -ExecutionPolicy Bypass -File sim-mesh\sim.ps1
$root = $PSScriptRoot
# Mounted as /mesh: sim mounts the clone's parent, where a firmware tree beside
# sim-mesh is found.
$outer = Split-Path -Parent $root
$engine = $env:SIM_MESH_ENGINE
if (-not $engine) {
    if (Get-Command podman -ErrorAction SilentlyContinue) { $engine = 'podman' }
    elseif (Get-Command docker -ErrorAction SilentlyContinue) { $engine = 'docker' }
    else { throw 'sim-mesh runs in a container, and neither podman nor docker is installed' }
}
& $engine info *> $null
if ($LASTEXITCODE -ne 0) { throw "$engine does not answer: is it running? (podman: podman machine start; docker: start Docker Desktop)" }
# sim is a bash script, which a checkout that turned its line ends into CRLF breaks.
if ((Get-Content -Raw (Join-Path $root 'sim')) -match "`r`n") {
    throw 'sim has Windows line ends: clone again with  git clone --config core.autocrlf=false https://github.com/sim-mesh/sim-mesh.git'
}
$tag = (Get-FileHash -Algorithm SHA256 (Join-Path $root 'Dockerfile')).Hash.Substring(0, 12).ToLower()
$image = "sim-mesh:win-$tag"
& $engine image inspect $image *> $null
if ($LASTEXITCODE -ne 0) {
    Write-Host "building the sim-mesh image ($image) with $engine..."
    & $engine build -t $image $root
    if ($LASTEXITCODE -ne 0) { throw 'the sim-mesh image did not build' }
}
# One front at a time, as sim keeps it: a running one is said so, one left
# behind stopped is cleared out of the name's way.
if (& $engine ps -q -f 'name=^sim-mesh-front$') { throw "a front is already running (sim-mesh-front): to restart it, $engine stop sim-mesh-front first" }
if (& $engine ps -aq -f 'name=^sim-mesh-front$') { & $engine rm -f sim-mesh-front | Out-Null }
Write-Host 'the page: http://localhost:8800/ (once the front says it is up; Ctrl-C stops it)'
& $engine run --rm -it --init --name sim-mesh-front `
    --sysctl net.ipv4.ip_unprivileged_port_start=0 `
    -p 8800:8800/tcp -p 8800:8800/udp `
    -v "${outer}:/mesh" -w /mesh/sim-mesh `
    -v sim-mesh-home:/home/sim-mesh -e HOME=/home/sim-mesh `
    -e SIM_MESH_IN_CONTAINER=1 -e SIM_MESH_CALLER_DIR=/mesh/sim-mesh `
    $image /mesh/sim-mesh/sim front
