#include "proxy.h"

void exports_wasi_http_incoming_handler_handle(
    exports_wasi_http_incoming_handler_own_incoming_request_t request,
    exports_wasi_http_incoming_handler_own_response_outparam_t response_out) {
  proxy_string_t path = {0};
  const bool has_path = wasi_http_types_method_incoming_request_path_with_query(
      wasi_http_types_borrow_incoming_request(request), &path);
  wasi_http_types_own_outgoing_response_t response =
      wasi_http_types_constructor_outgoing_response(
          wasi_http_types_constructor_fields());
  wasi_http_types_result_own_outgoing_response_error_code_t result = {0};
  result.val.ok = response;

  wasi_http_types_own_outgoing_body_t body;
  if (!wasi_http_types_method_outgoing_response_body(
          wasi_http_types_borrow_outgoing_response(response), &body))
    return;
  wasi_io_streams_own_output_stream_t stream;
  if (!wasi_http_types_method_outgoing_body_write(
          wasi_http_types_borrow_outgoing_body(body), &stream))
    return;
  wasi_http_types_static_response_outparam_set(response_out, &result);
  const char *payload = "c";
  size_t payload_len = 1;
  if (has_path && path.len >= 6 && __builtin_memcmp(path.ptr, "/hello", 6) == 0) {
    payload = "hello"; payload_len = 5;
  } else if (has_path && path.len >= 9 && __builtin_memcmp(path.ptr, "/language", 9) == 0) {
    payload = "c"; payload_len = 1;
  } else if (has_path && path.len >= 4 && __builtin_memcmp(path.ptr, "/cpu", 4) == 0) {
    payload = "c:387499?"; payload_len = 9;
  }
  wasi_io_streams_stream_error_t stream_error = {0};
  if (has_path && path.len >= 5 && __builtin_memcmp(path.ptr, "/echo", 5) == 0) {
    wasi_http_types_own_incoming_body_t incoming_body = {0};
    if (wasi_http_types_method_incoming_request_consume(
            wasi_http_types_borrow_incoming_request(request), &incoming_body)) {
      wasi_io_streams_own_input_stream_t input = {0};
      if (wasi_http_types_method_incoming_body_stream(
              wasi_http_types_borrow_incoming_body(incoming_body), &input)) {
        for (;;) {
          proxy_list_u8_t chunk = {0};
          if (!wasi_io_streams_method_input_stream_blocking_read(
                  wasi_io_streams_borrow_input_stream(input), 65536, &chunk,
                  &stream_error)) {
            proxy_list_u8_free(&chunk);
            break;
          }
          if (chunk.len == 0) {
            proxy_list_u8_free(&chunk);
            break;
          }
          wasi_io_streams_method_output_stream_blocking_write_and_flush(
              wasi_io_streams_borrow_output_stream(stream), &chunk,
              &stream_error);
          proxy_list_u8_free(&chunk);
        }
        wasi_io_streams_input_stream_drop_own(input);
      }
      wasi_http_types_own_future_trailers_t trailers =
          wasi_http_types_static_incoming_body_finish(incoming_body);
      wasi_io_poll_own_pollable_t pollable =
          wasi_http_types_method_future_trailers_subscribe(
              wasi_http_types_borrow_future_trailers(trailers));
      wasi_io_poll_method_pollable_block(
          wasi_io_poll_borrow_pollable(pollable));
      wasi_io_poll_pollable_drop_own(pollable);
      wasi_http_types_future_trailers_drop_own(trailers);
    }
  } else {
    proxy_list_u8_t bytes = {(uint8_t *)payload, payload_len};
    wasi_io_streams_method_output_stream_blocking_write_and_flush(
        wasi_io_streams_borrow_output_stream(stream), &bytes, &stream_error);
  }
  wasi_io_streams_output_stream_drop_own(stream);
  wasi_http_types_error_code_t finish_error = {0};
  wasi_http_types_static_outgoing_body_finish(body, 0, &finish_error);
  if (has_path) proxy_string_free(&path);
}
