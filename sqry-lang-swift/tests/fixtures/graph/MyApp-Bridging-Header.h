// Bridging header exposing C functions to Swift

#ifndef MyApp_Bridging_Header_h
#define MyApp_Bridging_Header_h

// C functions callable from Swift
void initialize_c_library(void);
int process_data(int value);
char* get_version(void);
void cleanup_resources(void);

#endif /* MyApp_Bridging_Header_h */
