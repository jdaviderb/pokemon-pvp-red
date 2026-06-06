/* logshim.c — a REAL C-variadic log callback for the libretro core.
 *
 * mupen64plus-next stores the fn pointer it gets from RETRO_ENVIRONMENT_GET_LOG_INTERFACE
 * and calls it (variadic) during retro_load_game. Declining that env call leaves the pointer
 * NULL and the core SIGSEGVs. Rust stable cannot define C-variadic fns, so we provide one in C
 * and hand its address to the core. (Harmless for parallel_n64, which may not use it.)
 */
#include <stdarg.h>
#include <stdio.h>

void n64_core_log(int level, const char *fmt, ...) {
    (void)level;
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
}
