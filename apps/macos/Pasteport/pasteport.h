/*
 * pasteport.h — C ABI for the Pasteport engine.
 *
 * Implemented by the pasteport-ffi crate, which builds as a static library.
 * Hand-written rather than generated: the surface is four functions, and a
 * checked-in header is easier to read in a diff than a build-time artifact.
 *
 * Ownership rules, in one place:
 *   - pasteport_version() returns a static string. Do NOT free it.
 *   - every other char* return is owned by the caller and must be released
 *     with pasteport_string_free().
 *   - a PasteportClient* must be released with pasteport_client_free().
 *
 * Thread safety: a PasteportClient* is NOT thread safe. Use one per thread, or
 * serialize access. The Swift side wraps it in an actor.
 */

#ifndef PASTEPORT_H
#define PASTEPORT_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque connection to the pasteportd control socket. */
typedef struct PasteportClient PasteportClient;

/*
 * Engine version, e.g. "0.1.0". Statically allocated; never free it.
 */
const char *pasteport_version(void);

/*
 * The default control socket path, or NULL if it cannot be determined.
 * Caller frees with pasteport_string_free().
 */
char *pasteport_default_socket_path(void);

/*
 * Connect to the daemon. Pass NULL for the default socket path.
 * Returns NULL when the daemon is not reachable.
 */
PasteportClient *pasteport_client_connect(const char *socket_path);

/*
 * Send one JSON request line, receive one JSON response.
 *
 * Never returns NULL for a non-NULL client: transport failures come back as
 * {"result":"error","message":"..."} so the caller has exactly one response
 * shape to decode.
 *
 * Caller frees the result with pasteport_string_free().
 */
char *pasteport_client_request(PasteportClient *client, const char *request_json);

/* Release a client. NULL is allowed. */
void pasteport_client_free(PasteportClient *client);

/* Release a string returned by this library. NULL is allowed. */
void pasteport_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* PASTEPORT_H */
