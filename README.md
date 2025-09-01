# Cascade

**Cascade will offer a flexible DNSSEC signing pipeline.** 

**A proof of concept (PoC) is scheduled to be available before October 2025,
followed by a production grade release in Q4 2025. Do NOT use the 
current codebase in production.**

If you have questions, suggestions or feature requests, don't hesitate to
[reach out](mailto:cascade@nlnetlabs.nl)!

## Pipeline Design

![cascade-pipeline 001](https://github.com/user-attachments/assets/0d9c599c-5362-4ee6-96bc-dc54de9c8c0f)

## HSM Support

Signing keys can either be BIND format key files or signing keys stored in a
KMIP compatible HSM, or PKCS#11 compatible HSM (via
[`kmip2pkcs11`](https://github.com/NLnetLabs/kmip2pkcs11)).

KMIP support is currently limited to that needed to communicate with
`kmip2pkcs11`.
