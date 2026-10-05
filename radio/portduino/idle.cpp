/**
 * The firmware's idle wait, and the descriptors that end it, for a Portduino
 * firmware. POSIX only: nothing of Arduino or Portduino.
 *
 *   rnode_idle(max_ms)        nothing to do for up to max_ms: ends then, on a
 *                             wake (a DIO1 rise is one), or when a watched
 *                             descriptor is readable
 *   rnode_idle_radio(max_ms)  the same without the watched descriptors, for a
 *                             wait inside a radio operation
 *   rnode_wake()              ends the idle now, or the next one
 *   rnode_watch_fd(fd), rnode_unwatch_fd(fd)
 *   rnode_idle_fd_ready()     whether the last rnode_idle ended on a watched
 *                             descriptor; cleared by asking
 *
 * Both waits are a poll(), one of the time shim's idle waits, which counts
 * the descriptors it waits on: a station is not idle while bytes wait for it.
 * A descriptor whose peer has gone is readable for ever, so the idle stops
 * watching it once it has reported it; it never reads one to find out.
 *
 * For a firmware linked with -Wl,--wrap=bind,--wrap=listen,--wrap=accept,
 * --wrap=close: a bind to INADDR_ANY binds SIM_MESH_BIND_ADDR, and a listening
 * or accepted socket is watched until it is closed. The __real_ functions are
 * weak, so a firmware linked without the wraps links this too.
 */
#include <arpa/inet.h>
#include <cerrno>
#include <climits>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <unistd.h>

void rnode_wake();
void rnode_idle(uint32_t max_ms);
void rnode_idle_radio(uint32_t max_ms);
void rnode_watch_fd(int fd);
void rnode_unwatch_fd(int fd);
int rnode_idle_fd_ready();

namespace {

const int MAX_WATCHED = 16;

pthread_mutex_t s_mu = PTHREAD_MUTEX_INITIALIZER;
pthread_once_t s_once = PTHREAD_ONCE_INIT;
int s_efd = -1;
int s_watched[MAX_WATCHED];
int s_nwatched;
bool s_fdReady;

void makeEfd()
{
    s_efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
}

int efd()
{
    pthread_once(&s_once, makeEfd);
    return s_efd;
}

void unwatchLocked(int fd)
{
    for (int i = 0; i < s_nwatched; i++) {
        if (s_watched[i] == fd) {
            s_watched[i] = s_watched[--s_nwatched];
            return;
        }
    }
}

void idle(uint32_t max_ms, bool watched)
{
    if (max_ms == 0)
        return;
    struct pollfd fds[1 + MAX_WATCHED];
    nfds_t n = 0;
    fds[n++] = {efd(), POLLIN, 0};
    if (watched) {
        pthread_mutex_lock(&s_mu);
        for (int i = 0; i < s_nwatched; i++)
            fds[n++] = {s_watched[i], (short)(POLLIN | POLLRDHUP), 0};
        pthread_mutex_unlock(&s_mu);
    }
    int rc = poll(fds, n, max_ms > (uint32_t)INT_MAX ? INT_MAX : (int)max_ms);
    bool ready = false;
    if (rc > 0) {
        if (fds[0].revents) {
            uint64_t count;
            while (read(fds[0].fd, &count, sizeof count) == (ssize_t)sizeof count) {}
        }
        pthread_mutex_lock(&s_mu);
        for (nfds_t i = 1; i < n; i++) {
            if (!fds[i].revents)
                continue;
            ready = true;
            if (fds[i].revents & (POLLRDHUP | POLLHUP | POLLERR | POLLNVAL))
                unwatchLocked(fds[i].fd);
        }
        pthread_mutex_unlock(&s_mu);
    }
    if (watched) {
        pthread_mutex_lock(&s_mu);
        s_fdReady = ready;
        pthread_mutex_unlock(&s_mu);
    }
}

bool bindAny(const struct sockaddr* addr, socklen_t len)
{
    if (!addr || addr->sa_family != AF_INET || len < (socklen_t)sizeof(struct sockaddr_in))
        return false;
    return ((const struct sockaddr_in*)addr)->sin_addr.s_addr == htonl(INADDR_ANY);
}

}  // namespace

void rnode_wake()
{
    uint64_t one = 1;
    ssize_t rc = write(efd(), &one, sizeof one);
    (void)rc;
}

void rnode_idle(uint32_t max_ms)
{
    idle(max_ms, true);
}

void rnode_idle_radio(uint32_t max_ms)
{
    idle(max_ms, false);
}

void rnode_watch_fd(int fd)
{
    if (fd < 0)
        return;
    pthread_mutex_lock(&s_mu);
    unwatchLocked(fd);
    if (s_nwatched < MAX_WATCHED)
        s_watched[s_nwatched++] = fd;
    pthread_mutex_unlock(&s_mu);
}

void rnode_unwatch_fd(int fd)
{
    pthread_mutex_lock(&s_mu);
    unwatchLocked(fd);
    pthread_mutex_unlock(&s_mu);
}

int rnode_idle_fd_ready()
{
    pthread_mutex_lock(&s_mu);
    bool ready = s_fdReady;
    s_fdReady = false;
    pthread_mutex_unlock(&s_mu);
    return ready ? 1 : 0;
}

extern "C" {

int __real_bind(int, const struct sockaddr*, socklen_t) __attribute__((weak));
int __real_listen(int, int) __attribute__((weak));
int __real_accept(int, struct sockaddr*, socklen_t*) __attribute__((weak));
int __real_close(int) __attribute__((weak));

int __wrap_bind(int fd, const struct sockaddr* addr, socklen_t len)
{
    const char* bind = getenv("SIM_MESH_BIND_ADDR");
    if (bind && *bind && bindAny(addr, len)) {
        struct sockaddr_in mine;
        memcpy(&mine, addr, sizeof mine);
        if (inet_pton(AF_INET, bind, &mine.sin_addr) == 1)
            return __real_bind(fd, (const struct sockaddr*)&mine, sizeof mine);
    }
    return __real_bind(fd, addr, len);
}

int __wrap_listen(int fd, int backlog)
{
    int rc = __real_listen(fd, backlog);
    if (rc == 0)
        rnode_watch_fd(fd);
    return rc;
}

int __wrap_accept(int fd, struct sockaddr* addr, socklen_t* len)
{
    int rc = __real_accept(fd, addr, len);
    if (rc >= 0)
        rnode_watch_fd(rc);
    return rc;
}

int __wrap_close(int fd)
{
    rnode_unwatch_fd(fd);
    return __real_close(fd);
}

}  // extern "C"
