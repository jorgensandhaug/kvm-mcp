# Security

Use GitHub's **Report a vulnerability** feature for private reports. Do not put
credentials, working transfer URLs or private device information in public issues.
If private reporting is unavailable, open an issue requesting a private contact
without disclosing exploit details.

Include the version, affected endpoint, reproduction with dummy credentials and
the expected versus observed behavior. This project has no guaranteed response
time or long-term support commitment yet.

Deploy behind private HTTPS. A bearer token grants hardware control, and a media
transfer URL grants its scoped file operation until consumed or expired. Do not
log those URLs or expose the service to untrusted networks. See the guide for
the threat boundaries and currently unverified hardware behavior.
