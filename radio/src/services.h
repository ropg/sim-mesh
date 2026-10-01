/**
 * services — what the model and its ether link are written against: the
 * host's services (simradio.h's `struct simradio_services`), the ones a host
 * handed over with `simradio_set_services`, else the library's own POSIX ones
 * (backend/posix).
 */
#pragma once

#include "simradio.h"

#ifdef __cplusplus
extern "C" {
#endif

/* The receive buffer `udp_open` asks of the kernel, in bytes. */
enum { kRecvBufferBytes = 1 << 20 };

/* The services in force: the host's, else the POSIX ones. */
const struct simradio_services* simradio_services(void);

/* The library's own services, for a plain POSIX process. */
const struct simradio_services* simradio_posix_services(void);

#ifdef __cplusplus
}
#endif
