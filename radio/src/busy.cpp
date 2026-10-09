/**
 * busy — SIM_MESH_BOARD's BUSY figures for one chip (busy.h).
 */
#include "busy.h"

#include "json.h"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>

BoardBusy readBoardBusy(const char* chip)
{
    BoardBusy b;
    const char* env = getenv("SIM_MESH_BOARD");
    simradio_json::Object board;
    if (!env || !board.parse(env, strlen(env))) return b;
    const std::string own = board.str("chip", "sx1262");
    if (own == chip) {           /* the board's own, measured with its chip */
        b.tcxo = board.num("busy_tcxo", 0) != 0;
        b.hostUs = (int32_t)std::max<int64_t>(0, board.num("host_us", 0));
    }
    const std::string spec = board.str("busy_us", "");
    size_t at = 0;
    while (at < spec.size()) {
        size_t end = spec.find(',', at);
        if (end == std::string::npos) end = spec.size();
        const std::string entry = spec.substr(at, end - at);
        at = end + 1;
        char name[48] = "";
        int us = -1;
        if (sscanf(entry.c_str(), " %47[^: ] : %d", name, &us) != 2 || us < 0) {
            fprintf(stderr, "simradio: SIM_MESH_BOARD busy_us entry \"%s\" is not "
                            "[<chip>.]<command>:<µs>; ignored\n", entry.c_str());
            continue;
        }
        /* Bare: the board's own chip's. Qualified: the chip it names. */
        std::string command = name;
        std::string whose = own;
        const size_t dot = command.find('.');
        if (dot != std::string::npos) {
            whose = command.substr(0, dot);
            command = command.substr(dot + 1);
        }
        if (whose == chip) b.entries.push_back({command, (int32_t)us});
    }
    return b;
}
