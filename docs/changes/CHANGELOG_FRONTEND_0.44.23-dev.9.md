# 0.44.23-dev.9

## Login links and clipboard

- Attach an explicit terminal hyperlink to the ChatGPT authorization URL so wrapped text retains its complete target where OpenTUI detects hyperlink support. Document its current Windows Terminal over SSH detection limitation.
- Add F2 in the login form to copy the full authorization URL through OSC 52, including over SSH, with visible success or local failure feedback. Callback entry stays concealed and untouched.
