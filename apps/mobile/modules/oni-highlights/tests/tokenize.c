#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "oni_highlights.h"

// Prints the records for argv[1] in language argv[2], one per line, from a
// second call so a reused instance is covered.
int main(int argc, char** argv) {
  if (argc != 3) return 2;
  FILE* file = fopen(argv[1], "rb");
  if (file == NULL) return 2;
  fseek(file, 0, SEEK_END);
  long length = ftell(file);
  fseek(file, 0, SEEK_SET);
  uint8_t* source = malloc(length > 0 ? (size_t)length : 1);
  if (fread(source, 1, (size_t)length, file) != (size_t)length) return 2;
  fclose(file);
  const uint32_t* records = NULL;
  int64_t count = 0;
  for (int call = 0; call < 2; call++) {
    count = oni_highlights_tokenize(source, (size_t)length, (uint8_t)atoi(argv[2]), &records);
    if (count < 0) return 1;
  }
  for (int64_t i = 0; i < count; i++) printf("%u\n", records[i]);
  return 0;
}
