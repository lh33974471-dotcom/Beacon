void* get_rgba();
void free_rgba(void*);
void set_by_name(const char*, const char*);
const char* get_by_name(const char*, const char*);

// iOS screen capture FFI (libs/scrap/src/ios/ffi.rs)
void ios_on_video_frame_update(const unsigned char* data, unsigned long len);
void ios_set_screen_size(unsigned short w, unsigned short h, unsigned short scale);
void ios_set_frame_raw_enable(const char* name, _Bool value);
void ios_set_frame_rotation(unsigned int degrees);

// Rust log callback (stub, kept for ABI compat)
typedef void (*RustLogCallback)(const char* msg);
void ios_set_log_callback(RustLogCallback cb);

void ios_set_managed_enrollment_token(const char* token);
void ios_set_managed_api_url(const char* url);

// Rust diagnostic state — returns a malloc'd C string, caller must free with ios_free_diagnostic_string
char* ios_get_diagnostic_state(void);
void ios_free_diagnostic_string(char* ptr);
